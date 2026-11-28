//! 백그라운드 엔진: 정확한 값 받기 → 공급자별 미터 → 토끼 상태 → 알림.
//!
//! 추정은 하지 않습니다. 화면에 나오는 숫자는 모두 서버가 준 값입니다.
//! - Claude: 파일 세션으로 HTTPS 사용량 조회, 필요하면 공식 CLI의 `/usage`.
//!           Claude Code 사용 중에는 상태 표시줄(rate_limits)에서도 값을 받습니다.
//! - Codex:  HTTPS 사용량 또는 CLI app-server 조회 (토큰 소모 없음) + Codex 로그.
//! - 로컬 Claude 기록: 쓰는 중인지(토끼 뛰기)와 한도 거절 메시지(낮잠)만.

use crate::accounts::{self, Account};
use crate::bridge::{self, BridgeFile, BridgeWindow};
use crate::usage_cmd;
use crate::usage_retry::{self, UsageError, UsageMode};
use crate::codex::{self, CodexReader, CodexSnapshot};
use crate::config::{self, Config, State};
use crate::fallback;
use crate::login::LoginStatus;
use crate::log_watch::{self, Changes, Interests, LogWatch};
use crate::logs::LogReader;

use crate::texts;
use crate::time::{self, DAY, HOUR};
use crate::tracker::{apply_exact, forecast_from, mood_of, Forecast, ForecastKind, Reading, Status, Tracker};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 기본 갱신 주기(분). 설정: 1 / 3 / 5 / 10 / 15
pub const DEFAULT_REFRESH_MIN: f64 = 5.0;
/// 리필권 만료 알림 기본 시점 (만료 몇 분 전)
pub const DEFAULT_REFILL_LEAD_MIN: f64 = 24.0 * 60.0;
/// 리필권 만료 알림 시점 범위 (1분 ~ 7일 전)
pub const MAX_REFILL_LEAD_MIN: f64 = 7.0 * 24.0 * 60.0;

/// 리필권 만료 알림 규칙 (켜짐, 만료 몇 분 전). 계정별 설정이 있으면 그것, 없으면 전체 설정
pub fn refill_rule(state: &State, account_id: &str) -> (bool, f64) {
    let on = state.pref_bool("refillNotify", false);
    let lead = state.pref_f64("refillLead").unwrap_or(DEFAULT_REFILL_LEAD_MIN);
    match state.0.get("refill_rules").and_then(|r| r.get(account_id)) {
        Some(r) => (
            r.get("on").and_then(|v| v.as_bool()).unwrap_or(on),
            r.get("lead").and_then(|v| v.as_f64()).unwrap_or(lead).clamp(1.0, MAX_REFILL_LEAD_MIN),
        ),
        None => (on, lead.clamp(1.0, MAX_REFILL_LEAD_MIN)),
    }
}
/// 쓰는 중이라 상태 표시줄·Codex 기록으로 새 값이 계속 들어오면 CLI 확인 간격을 이만큼 늘림
/// (모델별 주간 한도처럼 그쪽에 없는 값만 가끔 맞추면 됨)
const LIVE_STRETCH: f64 = 3.0;
/// 로그인 상태 확인 간격 (설정 창을 열거나 로그인·새로고침할 때는 바로 확인)
const LOGIN_CHECK: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderId {
    Claude,
    Codex,
}

impl ProviderId {
    pub fn name(self) -> &'static str {
        match self {
            ProviderId::Claude => "Claude",
            ProviderId::Codex => "Codex",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Meter {
    pub label: String,
    pub ratio: f64,
    pub resets_at: Option<f64>,
    /// 서버가 준 정확한 값인지 (false면 아직 값 없음 → "–")
    pub exact: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderView {
    pub id: ProviderId,
    pub account_id: String,
    /// "Claude" 또는 "Claude (회사)" 처럼 계정 이름 포함
    pub name: String,
    pub meters: Vec<Meter>,
    pub updated_at: Option<f64>,
    pub note: Option<String>,
    /// 낮잠 예보 한 줄 (Claude, 정확한 값이 있을 때만)
    pub forecast: Option<String>,
    /// 위젯에 표시
    pub show: bool,
    /// 계정 이메일·플랜 (Codex는 app-server가 알려 줌)
    pub email: Option<String>,
    pub workspace_id: Option<String>,
    pub plan: Option<String>,
    /// 크레딧(Codex)·리필권(Codex, Claude 한도 초기화권)
    pub credits: Option<codex::Credits>,
    pub refills: Option<codex::Refills>,
    /// Claude 추가 사용량·지출 한도 (보일 글자)
    pub extra: Option<String>,
    /// Claude 쿠폰 (만료되지 않은 것, 만료가 빠른 순)
    pub coupons: Vec<bridge::Coupon>,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub now: f64,
    /// 토끼(귀·기분)를 움직이는 Claude 상태 (위젯에 보이는 첫 Claude 계정)
    pub claude: Option<Status>,
    pub providers: Vec<ProviderView>,
    /// 계정별 로그인 상태 (로컬 파일 또는 공식 CLI로 확인)
    pub logins: HashMap<String, LoginStatus>,
    /// 계정별 사용량 확인 상태
    pub probes: HashMap<String, ProbeState>,
    pub(crate) revisions: HashMap<String, u64>,
    pub(crate) claude_statuses: HashMap<String, Status>,
}

impl Snapshot {
    pub(crate) fn retain_current(&mut self, list: &[Account]) {
        let previous_rabbit = self.providers.iter()
            .filter(|p| p.id == ProviderId::Claude)
            .min_by_key(|p| !p.show)
            .map(|p| p.account_id.clone());
        self.revisions.retain(|id, revision| list.iter().any(|a| &a.id == id && a.revision == *revision));
        self.providers.retain(|p| self.revisions.contains_key(&p.account_id));
        self.logins.retain(|id, _| self.revisions.contains_key(id));
        self.probes.retain(|id, _| self.revisions.contains_key(id));
        self.claude_statuses.retain(|id, _| self.revisions.contains_key(id));
        if previous_rabbit.is_some_and(|id| !self.revisions.contains_key(&id)) {
            self.claude = None;
        }
    }

    pub fn provider(&self, id: ProviderId) -> Option<&ProviderView> {
        self.providers.iter().find(|p| p.id == id)
    }
    pub fn visible(&self) -> impl Iterator<Item = &ProviderView> {
        self.providers.iter().filter(|p| p.show)
    }

    /// 공식 CLI가 로그아웃이라고 한 계정 (확인 실패로 모를 때는 false)
    pub fn logged_out(&self, account_id: &str) -> bool {
        self.logins.get(account_id).and_then(|l| l.logged_in) == Some(false)
    }
}

/// 사용량 확인(Claude 확인 요청 / Codex app-server) 상태
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProbeState {
    pub running: bool,
    /// 마지막으로 시도한 시각 (성공/실패 무관)
    pub tried_at: Option<f64>,
    pub retry_at: Option<f64>,
    pub next_at: Option<f64>,
    pub error: Option<String>,
}

impl ProbeState {
    fn finished(&mut self, result: Result<(), UsageError>, now: f64, retry_at: Option<f64>) {
        self.running = false;
        self.retry_at = retry_at;
        if result.as_ref().err().is_none_or(UsageError::attempted) {
            self.tried_at = Some(now);
            self.error = result.err().map(|error| error.to_string());
        } else if matches!(result, Err(UsageError::Deferred)) {
            self.error = Some(UsageError::Deferred.to_string());
        }
    }
}

/// 갱신 주기(분)
pub fn refresh_minutes(state: &State) -> f64 {
    state.pref_f64("refresh_minutes").unwrap_or(DEFAULT_REFRESH_MIN).clamp(1.0, 60.0)
}

/// 공식 CLI로 사용량을 다시 받을 때인지 (`/usage`, `codex app-server` 모두 토큰 0이지만 CLI를 띄우는 비용이 있음).
/// - `tried`: 마지막으로 CLI로 확인한 시각
/// - `live_at`: 상태 표시줄(Claude)·실행 기록(Codex)으로 마지막 값이 들어온 시각
/// 마지막 확인 뒤에 쓰는 중인 값이 따로 들어왔으면 이미 최신이므로 간격을 늘림.
pub fn usage_due(tried: Option<f64>, live_at: Option<f64>, interval_min: f64, now: f64) -> bool {
    tried.is_none_or(|tried| now >= next_usage_at(tried, live_at, interval_min))
}

fn next_usage_at(tried: f64, live_at: Option<f64>, interval_min: f64) -> f64 {
    let gap = interval_min * 60.0;
    let regular = tried + gap - 5.0;
    live_at.filter(|at| *at > tried + 5.0).map_or(regular, |at| {
        regular.max((tried + gap * LIVE_STRETCH - 5.0).min(at + gap))
    })
}

fn scheduled_usage_at(
    regular_at: f64,
    resets: &[f64],
    checked_through: f64,
    now: f64,
) -> (f64, UsageMode) {
    let reset_at = resets.iter().copied()
        .filter(|at| at.is_finite() && *at > checked_through)
        .fold(f64::INFINITY, f64::min);
    if regular_at > now && reset_at < regular_at {
        (reset_at, UsageMode::ApiOnly)
    } else {
        (regular_at, UsageMode::WithCli)
    }
}

fn checked_quota_through(
    started: f64,
    finished: f64,
    mut reported_resets: impl Iterator<Item = f64>,
) -> f64 {
    // A request that straddled a reset may still return the expired window.
    if reported_resets.any(|at| at > started && at <= finished) {
        started
    } else {
        finished
    }
}

fn claude_reset_times(file: &BridgeFile) -> impl Iterator<Item = f64> + '_ {
    file.five_hour.into_iter()
        .chain(file.seven_day.filter(|_| !file.weekly_unavailable))
        .chain(file.models.iter().map(|model| model.window))
        .filter_map(|window| window.resets_at)
}

fn codex_reset_times(snapshot: &CodexSnapshot) -> impl Iterator<Item = f64> + '_ {
    snapshot.windows.iter().filter_map(|window| window.resets_at)
}

/// Claude 확인 요청을 보낼지. 5시간 창은 첫 요청부터 시작하므로 창이 닫혀 있으면 자동으로 보내지 않음
/// (처음 한 번과 사용자가 누른 새로고침만 예외).
pub fn probe_due(file: Option<&BridgeFile>, tried_at: Option<f64>, interval_min: f64, now: f64) -> bool {
    let since_try = tried_at.map(|t| now - t).unwrap_or(f64::INFINITY);
    let Some(f) = file else {
        return since_try > 600.0; // 처음 한 번 (실패하면 10분 뒤 다시)
    };
    if f.five_hour.and_then(|w| w.live_reset(now)).is_none() {
        return false; // 창이 닫혀 있음 → 0%로 확정, 요청 안 보냄
    }
    let interval = interval_min * 60.0;
    now - f.updated_at >= interval - 5.0 && since_try >= interval - 5.0
}

fn display_name(id: ProviderId, acc: &Account) -> String {
    if acc.label.trim().is_empty() {
        id.name().to_string()
    } else {
        format!("{} ({})", id.name(), acc.label.trim())
    }
}

pub fn age_text(secs: f64) -> String {
    if secs < 90.0 {
        crate::tr!("방금", "just now").into()
    } else {
        let age = texts::fmt_duration(secs);
        crate::tr!(format!("{age} 전"), format!("{age} ago"))
    }
}

/// 카드에 보이는 값 시점: "3분 전 기준" / "Updated 3m ago"
fn updated_note(secs: f64) -> String {
    let age = age_text(secs);
    crate::tr!(format!("{age} 기준"), format!("Updated {age}"))
}

pub fn claude_view(acc: &Account, st: &Status, file: Option<&BridgeFile>) -> ProviderView {
    let current = |window: Option<BridgeWindow>| window.is_some_and(|w| w.resets_at.is_none_or(|r| r > st.now));
    let session_known = st.nap_kind == Some(crate::logs::LimitKind::Session)
        || current(file.and_then(|b| b.five_hour));
    let weekly_known = st.nap_kind == Some(crate::logs::LimitKind::Weekly)
        || file.is_some_and(|b| !b.weekly_unavailable && current(b.seven_day));
    let updated_at = file.map(|b| b.updated_at);
    ProviderView {
        id: ProviderId::Claude,
        account_id: acc.id.clone(),
        name: display_name(ProviderId::Claude, acc),
        meters: vec![
            Meter { label: "5시간".into(), ratio: st.session_pct, resets_at: st.session_reset_at.filter(|_| session_known), exact: session_known },
            Meter { label: "주간".into(), ratio: st.weekly_pct, resets_at: st.weekly_reset_at.filter(|_| weekly_known), exact: weekly_known },
        ],
        updated_at,
        note: updated_at.map(|t| updated_note(st.now - t)),
        forecast: if st.exact { Some(texts::forecast_line(st)).filter(|f| !f.is_empty()) } else { None },
        show: acc.show,
        email: None,
        workspace_id: None,
        plan: None,
        credits: None,
        refills: None,
        extra: None,
        coupons: Vec::new(),
    }
}

pub fn codex_view(acc: &Account, s: &CodexSnapshot, now: f64) -> ProviderView {
    ProviderView {
        id: ProviderId::Codex,
        account_id: acc.id.clone(),
        // 이름을 따로 안 정했으면 이메일 앞부분으로 구분 (ChatGPT 계정 여러 개)
        name: match (&s.email, acc.label.trim().is_empty()) {
            (Some(e), true) => format!("{} ({})", ProviderId::Codex.name(), e.split('@').next().unwrap_or(e)),
            _ => display_name(ProviderId::Codex, acc),
        },
        meters: s
            .windows
            .iter()
            .map(|w| Meter {
                label: codex::window_label(w.minutes),
                ratio: w.ratio_at(now),
                resets_at: w.next_reset(now),
                exact: w.resets_at.is_none_or(|r| r > now),
            })
            .collect(),
        updated_at: Some(s.ts),
        note: Some(updated_note(now - s.ts)),
        forecast: None,
        show: acc.show,
        email: s.email.clone(),
        workspace_id: s.workspace_id.clone(),
        plan: s.plan.clone(),
        credits: s.credits.clone(),
        refills: s.refills.clone(),
        extra: None,
        coupons: Vec::new(),
    }
}

// ------------------------------------------------------------------ 알림

/// 사용량별 알림 설정 키
pub fn alert_key(account_id: &str, label: &str) -> String {
    format!("{account_id}:{label}")
}

/// 리셋 임박 알림 시점의 전체 상한 (7일)
pub const MAX_RESET_LEAD_MIN: u32 = 7 * 24 * 60;

/// 저장 키의 라벨은 번역 전의 그래프 주기 (5시간, 주간, Fable 주간 등).
pub fn reset_lead_max(key: &str) -> u32 {
    let label = key.split_once(':').map_or("", |(_, label)| label);
    [("분", 1), ("시간", 60), ("일", 1440)]
        .into_iter()
        .find_map(|(unit, scale)| {
            let count = label.strip_suffix(unit)?.parse::<u32>().ok()?;
            Some(count.saturating_mul(scale).min(MAX_RESET_LEAD_MIN))
        })
        .unwrap_or(MAX_RESET_LEAD_MIN)
}

/// 사용량 기준 범위 (%)
pub fn pct_in_range(v: f64) -> bool {
    (1.0..100.0).contains(&v)
}

/// 리셋 임박 시점 범위 (리셋 몇 분 전)
pub fn lead_in_range(v: f64, maximum: u32) -> bool {
    (1.0..=f64::from(maximum)).contains(&v)
}

/// 그래프 하나의 알림 설정.
/// alerts["계정:라벨"] = {"on": 그래프 전체, "at": [사용량 %], "at_off": [꺼 둔 사용량 %],
/// "before": [리셋 몇 분 전], "before_off": [꺼 둔 리셋 임박 시점]}
#[derive(Clone, Debug, PartialEq)]
pub struct AlertSet {
    /// 그래프 전체 알림 (기본 켜짐)
    pub on: bool,
    /// 켜 둔 사용량 기준 (%)
    pub at: Vec<f64>,
    /// 꺼 둔 사용량 기준 (남겨 두지만 알리지 않음)
    pub at_off: Vec<f64>,
    /// 켜 둔 리셋 임박 시점 (리셋 몇 분 전)
    pub before: Vec<f64>,
    /// 꺼 둔 리셋 임박 시점
    pub before_off: Vec<f64>,
}

/// 알림 목록 하나: 숫자 배열 (예전 형식 "at": 80처럼 숫자 하나도 읽음), 범위 밖 값은 버리고 정렬·중복 제거
fn alert_list(rule: Option<&serde_json::Value>, field: &str, ok: impl Fn(f64) -> bool) -> Vec<f64> {
    let mut v: Vec<f64> = match rule.and_then(|r| r.get(field)) {
        Some(serde_json::Value::Array(a)) => a.iter().filter_map(|v| v.as_f64()).collect(),
        Some(v) => v.as_f64().into_iter().collect(),
        None => Vec::new(),
    };
    v.retain(|&x| ok(x));
    v.sort_by(|a, b| a.total_cmp(b));
    v.dedup();
    v
}

/// 그래프 알림 설정 읽기. 기본은 기준 없음: 사용자가 그래프에 추가한 기준에서만 알림.
/// 켜 둔 목록과 꺼 둔 목록에 같은 값이 있으면 켜 둔 쪽으로 봄.
pub fn alert_set(state: &State, key: &str) -> AlertSet {
    let rule = state.0.get("alerts").and_then(|a| a.get(key));
    let lead_max = reset_lead_max(key);
    let valid_lead = |v| lead_in_range(v, lead_max);
    let at = alert_list(rule, "at", pct_in_range);
    let before = alert_list(rule, "before", valid_lead);
    let mut at_off = alert_list(rule, "at_off", pct_in_range);
    at_off.retain(|v| !at.contains(v));
    let mut before_off = alert_list(rule, "before_off", valid_lead);
    before_off.retain(|v| !before.contains(v));
    AlertSet {
        on: rule.and_then(|r| r.get("on")).and_then(|v| v.as_bool()).unwrap_or(true),
        at,
        at_off,
        before,
        before_off,
    }
}

/// 알림 규칙: (그래프 전체 켜짐, 켜 둔 사용량 기준 %)
pub fn alert_rule(state: &State, key: &str) -> (bool, Vec<f64>) {
    let s = alert_set(state, key);
    (s.on, s.at)
}

/// 계정 순서대로 (목록에 없는 건 뒤로, 원래 순서 유지)
pub fn sort_by_order(providers: &mut [ProviderView], order: &[String]) {
    providers.sort_by_key(|p| order.iter().position(|id| *id == p.account_id).unwrap_or(usize::MAX));
}

pub struct Notices {
    pub state: State,
}

impl Notices {
    pub fn load() -> Self {
        Self { state: State::load() }
    }

    /// 보낼 알림 (제목, 내용). 같은 알림은 창(리셋 시각)마다 한 번.
    pub fn check(&mut self, prev: Option<&Snapshot>, snap: &Snapshot, cfg: &Config) -> Vec<(String, String)> {
        if !cfg.notify {
            return Vec::new(); // 전체 알림 끔
        }
        let mut out = Vec::new();
        let mut dirty = false;
        // 한도 도달·리셋은 선택 사항. 로그인 만료 알림은 기본으로 켬.
        let nap_on = self.state.pref_bool("napNotify", false);
        let login_on = self.state.pref_bool("loginNotify", true);
        for (id, st) in snap.claude_statuses.iter().filter(|_| nap_on) {
            let Some(provider) = snap.providers.iter().find(|p| p.account_id == *id) else { continue };
            let state_key = format!("nap:{id}");
            if let (Some(kind), Some(wake)) = (st.nap_kind, st.wake_at) {
                let key = format!("{}:{kind:?}:{}", snap.revisions.get(id).copied().unwrap_or(0), wake as i64);
                if self.state.get_str(&state_key) != Some(key.as_str()) {
                    self.state.set_str(&state_key, &key);
                    dirty = true;
                    let (title, body) = texts::nap_notice(st);
                    out.push((title, format!("{} · {body}", provider.name)));
                }
            } else if prev.is_some_and(|previous| {
                previous.revisions.get(id) == snap.revisions.get(id)
                    && previous.claude_statuses.get(id).and_then(|status| status.wake_at)
                        .is_some_and(|wake| wake <= snap.now)
            }) {
                let (title, body) = texts::wake_notice();
                out.push((title, format!("{} · {body}", provider.name)));
            }
        }
        // Warn once per login. If no advance warning was sent, notify on expiry.
        for (id, l) in snap.logins.iter().filter(|_| login_on) {
            let key = format!("login:{id}");
            let warning_key = format!("login-expiry:{id}");
            let revision = snap.revisions.get(id).copied().unwrap_or(0);
            let warned_expiry = self.state.get_str(&warning_key)
                .and_then(|stamp| stamp.split_once(':'))
                .filter(|(rev, _)| rev.parse::<u64>().ok() == Some(revision))
                .and_then(|(_, at)| at.parse::<i64>().ok());
            let already_warned = warned_expiry.is_some_and(|at| {
                // Renewing outside usAgI can leave the revision unchanged. A newer
                // end time identifies the new login, while a shortened end time
                // on the same login does not cause a second warning.
                l.sign_in_expiry.is_none_or(|expiry| expiry.expires_at <= at as f64 + 60.0)
            });
            let name = || snap.providers.iter().find(|p| &p.account_id == id)
                .map(|p| p.name.as_str())
                .unwrap_or_else(|| if id.starts_with("codex") { "Codex" } else { "Claude" });
            match l.logged_in {
                Some(false) if self.state.get_str(&key) != Some("expired") => {
                    self.state.set_str(&key, "expired");
                    dirty = true;
                    if !already_warned {
                        let name = name();
                        out.push((
                            crate::tr!("로그인 필요", "Sign-in required").into(),
                            crate::tr!(
                                format!("{name} 로그인 만료 · usAgI 설정에서 다시 연결"),
                                format!("{name} sign-in expired. Reconnect in usAgI settings.")
                            ),
                        ));
                    }
                }
                Some(true) => {
                    dirty |= self.state.0.remove(&key).is_some();
                    if !already_warned {
                        dirty |= self.state.0.remove(&warning_key).is_some();
                        if let Some(expiry) = l.sign_in_expiry.filter(|expiry| expiry.warning_due(snap.now)) {
                            self.state.set_str(&warning_key, &format!("{revision}:{}", expiry.expires_at as i64));
                            dirty = true;
                            let (name, end) = (name(), texts::fmt_clock(expiry.expires_at, snap.now));
                            out.push((
                                crate::tr!("로그인 만료 임박", "Sign-in expiring soon").into(),
                                crate::tr!(
                                    format!("{name} 로그인 만료 예정 ({end}) · 설정에서 다시 연결"),
                                    format!("{name} sign-in ends by {end}. Reconnect in settings.")
                                ),
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
        // 리필권 만료 임박: 정한 시간 전부터, 만료 시각마다 한 번 (계정별 설정 우선)
        for p in &snap.providers {
            let Some(r) = p.refills.as_ref().filter(|r| r.available > 0) else { continue };
            let (on, lead) = refill_rule(&self.state, &p.account_id);
            let Some(at) = r.expires_at.filter(|&t| on && t > snap.now && t - snap.now <= lead * 60.0) else { continue };
            let key = format!("refill:{}", p.account_id);
            let stamp = ((at / 60.0).round() as i64).to_string();
            if self.state.get_str(&key) == Some(stamp.as_str()) {
                continue;
            }
            self.state.set_str(&key, &stamp);
            dirty = true;
            let (n, left, clock) = (r.available, texts::fmt_duration(at - snap.now), texts::fmt_clock(at, snap.now));
            out.push((
                crate::tr!("리필권 만료 임박", "Refill expiring soon").into(),
                crate::tr!(
                    format!("{} 리필권 {n}장 · {left} 뒤 만료 ({clock})", p.name),
                    format!("{}: {n} refill{} expire in {left} ({clock})", p.name, if n == 1 { "" } else { "s" })
                ),
            ));
        }
        // Claude 쿠폰 만료 임박: 리필권과 같은 설정, 가장 먼저 만료되는 쿠폰의 만료 시각마다 한 번
        for p in &snap.providers {
            let Some(c) = p.coupons.iter().filter(|c| c.expires_at > snap.now).min_by(|a, b| a.expires_at.total_cmp(&b.expires_at)) else { continue };
            let (on, lead) = refill_rule(&self.state, &p.account_id);
            let at = c.expires_at;
            if !on || at - snap.now > lead * 60.0 {
                continue;
            }
            let key = format!("coupon:{}", p.account_id);
            let stamp = ((at / 60.0).round() as i64).to_string();
            if self.state.get_str(&key) == Some(stamp.as_str()) {
                continue;
            }
            self.state.set_str(&key, &stamp);
            dirty = true;
            let (amount, left, clock) = (c.amount_label(), texts::fmt_duration(at - snap.now), texts::fmt_clock(at, snap.now));
            out.push((
                crate::tr!("쿠폰 만료 임박", "Coupon expiring soon").into(),
                crate::tr!(
                    format!("{} 쿠폰 {amount} · {left} 뒤 만료 ({clock})", p.name),
                    format!("{}: {amount} coupon expires in {left} ({clock})", p.name)
                ),
            ));
        }
        // 사용량별 알림: 넘은 기준 중 가장 높은 것 하나만, 창마다 한 번
        for p in &snap.providers {
            for m in p.meters.iter().filter(|m| m.exact) {
                let key = alert_key(&p.account_id, &m.label);
                let (on, thresholds) = alert_rule(&self.state, &key);
                if !on {
                    continue;
                }
                let Some(&hit) = thresholds.iter().rev().find(|&&t| m.ratio >= t / 100.0) else { continue };
                let bucket = m.resets_at.map(|r| (r / 600.0).round() as i64).unwrap_or((snap.now / DAY) as i64);
                let seen_key = format!("tired:{key}");
                let prev_seen = self.state.get_str(&seen_key).unwrap_or("").to_string();
                // 같은 창에서 이미 이 기준 이상을 알렸으면 생략
                let already = prev_seen
                    .split_once(':')
                    .is_some_and(|(b, t)| b == bucket.to_string() && t.parse::<f64>().is_ok_and(|t| t >= hit));
                if !already {
                    self.state.set_str(&seen_key, &format!("{bucket}:{hit}"));
                    dirty = true;
                    let st = snap.claude_statuses.get(&p.account_id);
                    out.push(texts::tired_notice(&p.name, &m.label, m.ratio, st));
                }
            }
        }
        // 리셋 임박: 켜 둔 시점 중 지금 맞는 가장 작은 것 하나만, 창마다 한 번
        for p in &snap.providers {
            for m in p.meters.iter().filter(|m| m.exact && m.ratio > 0.0) {
                let Some(reset) = m.resets_at.filter(|&r| r > snap.now) else { continue };
                let key = alert_key(&p.account_id, &m.label);
                let set = alert_set(&self.state, &key);
                if !set.on {
                    continue;
                }
                // before는 오름차순이라 처음 맞는 값이 가장 작은 시점
                let Some(&hit) = set.before.iter().find(|&&b| reset - snap.now <= b * 60.0) else { continue };
                let stamp = (reset / 60.0).round() as i64;
                let seen_key = format!("soon:{key}");
                // 같은 창(리셋 시각 10분 안쪽)에서 이 시점 이하를 이미 알렸으면 생략 → 더 큰 시점도 보낸 것으로 봄
                let already = self.state.get_str(&seen_key).and_then(|s| s.split_once(':')).is_some_and(|(r, b)| {
                    r.parse::<i64>().is_ok_and(|r| (r - stamp).abs() <= 10) && b.parse::<f64>().is_ok_and(|b| b <= hit)
                });
                if !already {
                    self.state.set_str(&seen_key, &format!("{stamp}:{hit}"));
                    dirty = true;
                    out.push(texts::soon_notice(&p.name, &m.label, reset, snap.now));
                }
            }
        }
        let compound = self.compound_usage(snap, snap.now);
        dirty |= !compound.is_empty();
        out.extend(compound);
        if dirty {
            self.state.save();
        }
        out
    }

    pub(crate) fn compound_usage(&mut self, snap: &Snapshot, now: f64) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for p in &snap.providers {
            for m in &p.meters {
                let key = alert_key(&p.account_id, &m.label);
                for rule in crate::alert_rules::due(&mut self.state, &key, m, now) {
                    let details = rule.conditions.iter().map(|c| {
                        format!("{} {}", c.metric.label(), c.description())
                    }).collect::<Vec<_>>().join(match rule.logic {
                        crate::alert_rules::Logic::And => crate::tr!(" · 그리고 · ", " · and · "),
                        crate::alert_rules::Logic::Or => crate::tr!(" · 또는 · ", " · or · "),
                    });
                    out.push((crate::tr!("사용량 알림", "Usage alert").into(),
                        format!("{} · {}\n{details}", p.name, texts::meter_label(&m.label))));
                }
            }
        }
        out
    }
}

// ------------------------------------------------------------------ 소스

struct ClaudeSource {
    acc: Account,
    reader: LogReader,
    usage_path: PathBuf,
    usage: Option<BridgeFile>,
    tracker: Tracker,
    /// 정확한 값 기록 (낮잠 예보용)
    five: Vec<Reading>,
    seven: Vec<Reading>,
    last_seen: f64,
}

struct CodexSource {
    acc: Account,
    reader: CodexReader,
    usage_path: PathBuf,
    usage: Option<CodexSnapshot>,
}

impl CodexSource {
    fn latest(&self) -> Option<&CodexSnapshot> {
        // The execution log wins when its timestamp matches the saved API response.
        self.usage.iter().chain(self.reader.snapshot.iter()).max_by(|a, b| a.ts.total_cmp(&b.ts))
    }
}

struct Sources {
    claude: Vec<ClaudeSource>,
    codex: Vec<CodexSource>,
    /// 설정 화면에서 정한 계정 순서 (위젯·메뉴 막대도 이 순서)
    order: Vec<String>,
    accounts: Vec<Account>,
}

impl Sources {
    fn reconfigure(&mut self, cfg: &Config, accounts: &[Account], active: &HashMap<String, PathBuf>) {
        let mut updated = Self::build(cfg, accounts, active);
        let mut claude: HashMap<_, _> = std::mem::take(&mut self.claude).into_iter()
            .map(|source| (source.acc.usage_key(), source)).collect();
        let mut codex: HashMap<_, _> = std::mem::take(&mut self.codex).into_iter()
            .map(|source| (source.acc.usage_key(), source)).collect();
        for source in &mut updated.claude {
            if let Some(previous) = claude.remove(&source.acc.usage_key()).filter(|p| p.reader.roots == source.reader.roots) {
                *source = ClaudeSource { acc: source.acc.clone(), ..previous };
            }
        }
        for source in &mut updated.codex {
            if let Some(previous) = codex.remove(&source.acc.usage_key()).filter(|p| p.reader.roots == source.reader.roots) {
                source.reader = previous.reader;
            }
        }
        *self = updated;
    }

    /// active: 연결별로 주 폴더 대신 쓰는 폴더.
    fn build(cfg: &Config, accounts: &[Account], active: &HashMap<String, PathBuf>) -> Self {
        let mut claude = Vec::new();
        let mut codex = Vec::new();
        for acc in accounts {
            let extra = active.get(&acc.usage_key()).filter(|dir| accounts::fallback_profile_available(dir));
            match acc.provider.as_str() {
                "claude" if cfg.providers.iter().any(|p| p == "claude") => {
                    // 대체 폴더는 조회 때만 사용합니다. 로컬 기록에는 계정 식별자가 없습니다.
                    let mut roots = vec![acc.path().join("projects")];
                    if crate::login::same_dir(&acc.path(), accounts::default_claude_dir()) {
                        roots.extend(cfg.claude_dirs.iter().map(PathBuf::from));
                    }
                    claude.push(ClaudeSource {
                        acc: acc.clone(),
                        reader: LogReader::for_connection(roots.into_iter().map(log_watch::watch_path).collect(), acc.revision),
                        usage_path: log_watch::watch_path(bridge::path(&acc.usage_key())),
                        usage: None,
                        tracker: Tracker::new(),
                        five: Vec::new(),
                        seven: Vec::new(),
                        last_seen: 0.0,
                    });
                }
                "codex" if cfg.providers.iter().any(|p| p == "codex") => codex.push(CodexSource {
                    acc: acc.clone(),
                    reader: CodexReader::for_connection(
                        if extra.is_some() {
                            // 대체 폴더의 로그에는 워크스페이스 정보가 없어 내려받은 값만 표시.
                            Vec::new()
                        } else {
                            ["sessions", "archived_sessions"].into_iter()
                                .map(|sub| log_watch::watch_path(acc.path().join(sub))).collect()
                        },
                        acc.revision,
                    ),
                    usage_path: log_watch::watch_path(codex::saved_path(&acc.usage_key())),
                    usage: None,
                }),
                _ => {}
            }
        }
        Sources { claude, codex, order: accounts.iter().map(|a| a.id.clone()).collect(), accounts: accounts.to_vec() }
    }

    fn watch_interests(&self) -> Interests {
        Interests {
            logs: self.claude.iter().flat_map(|s| &s.reader.roots)
                .chain(self.codex.iter().flat_map(|s| &s.reader.roots)).cloned().collect(),
            usage: self.claude.iter().map(|s| &s.usage_path)
                .chain(self.codex.iter().map(|s| &s.usage_path)).cloned().collect(),
        }
    }

    fn read_changes(&mut self, changes: &Changes) {
        for src in &mut self.claude {
            let paths = changes.log_paths(&src.reader.roots);
            let mut changed = !paths.is_empty() && src.reader.update_paths(paths);
            if changes.needs_scan(&src.reader.roots) {
                changed |= src.reader.rescan();
            }
            if changed {
                let limits = src.reader.limit_events();
                src.tracker.update(src.reader.usage_events(), limits);
            }
            if changes.rescan || changes.paths.contains_key(&src.usage_path) {
                src.usage = bridge::read(&src.acc.usage_key());
            }
        }
        for src in &mut self.codex {
            let paths = changes.log_paths(&src.reader.roots);
            if !paths.is_empty() {
                src.reader.update_paths(paths);
            }
            if changes.needs_scan(&src.reader.roots) {
                src.reader.rescan();
            }
            if changes.rescan || changes.paths.contains_key(&src.usage_path) {
                src.usage = codex::load(&src.acc.usage_key());
            }
        }
    }

    fn rescan(&mut self) {
        self.read_changes(&Changes { rescan: true, ..Changes::default() });
    }

    fn usage_saved(&mut self, key: &str) {
        for src in self.claude.iter_mut().filter(|s| s.acc.usage_key() == key) {
            src.usage = bridge::read(key);
        }
        for src in self.codex.iter_mut().filter(|s| s.acc.usage_key() == key) {
            src.usage = codex::load(key);
        }
    }

    fn reset_times(&self) -> HashMap<String, Vec<f64>> {
        let claude = self.claude.iter().filter_map(|src| {
            let file = src.usage.as_ref()?;
            Some((src.acc.usage_key(), claude_reset_times(file).collect()))
        });
        let codex = self.codex.iter().filter_map(|src| {
            let latest = src.latest()?;
            Some((src.acc.usage_key(), codex_reset_times(latest).collect()))
        });
        claude.chain(codex).collect()
    }

    fn snapshot(&mut self, cfg: &Config) -> Snapshot {
        let now = time::now();
        let tired = cfg.tired_percent / 100.0;
        let mut providers = Vec::new();
        let mut claude_statuses = HashMap::new();
        let mut rabbit: Option<(bool, Status)> = None;
        for src in &mut self.claude {
            let mut st = src.tracker.status(now);
            let file = src.usage.as_ref();
            if let Some(b) = file {
                apply_exact(&mut st, b, tired);
                // 새 값이 들어오면 예보용 기록에 추가 (최근 8시간)
                if b.updated_at > src.last_seen {
                    src.last_seen = b.updated_at;
                    if let Some(w) = b.five_hour {
                        src.five.push(Reading { at: b.updated_at, ratio: w.ratio_at(b.updated_at), reset: w.resets_at });
                    }
                    if let Some(w) = b.seven_day {
                        src.seven.push(Reading { at: b.updated_at, ratio: w.ratio_at(b.updated_at), reset: w.resets_at });
                    }
                    src.five.retain(|r| r.at > now - 8.0 * HOUR);
                    src.seven.retain(|r| r.at > now - 8.0 * HOUR);
                }
                st.forecast = pick_forecast(
                    forecast_from(&src.five, ForecastKind::Session, HOUR, now),
                    forecast_from(&src.seven, ForecastKind::Weekly, 6.0 * HOUR, now),
                );
                st.mood = mood_of(&st, tired);
            }
            let mut view = claude_view(&src.acc, &st, file);
            view.extra = file.and_then(|b| b.spend.as_ref().map(|s| s.label()).or_else(|| b.extra.clone()));
            view.coupons = file.and_then(|b| b.coupons.as_ref()).into_iter().flatten()
                .filter(|c| c.expires_at > now).cloned().collect();
            view.coupons.sort_by(|a, b| a.expires_at.total_cmp(&b.expires_at));
            view.refills = file.and_then(|b| b.refills.clone()).filter(|r| r.expires_at.is_none_or(|at| at > now));
            if let Some(b) = file.filter(|_| st.exact) {
                for m in &b.models {
                    view.meters.push(Meter {
                        label: format!("{} 주간", m.name),
                        ratio: m.window.ratio_at(now),
                        resets_at: m.window.live_reset(now),
                        exact: m.window.resets_at.is_none_or(|r| r > now),
                    });
                }
            }
            providers.push(view);
            claude_statuses.insert(src.acc.id.clone(), st.clone());
            // 토끼는 위젯에 보이는 첫 Claude 계정을 따라감
            let better = match &rabbit {
                None => true,
                Some((shown, _)) => !shown && src.acc.show,
            };
            if better {
                rabbit = Some((src.acc.show, st));
            }
        }
        for src in &self.codex {
            // 로그 값과 직접 조회 값 중 최신
            if let Some(mut s) = src.latest().cloned() {
                if let Some(cache) = &src.usage {
                    s.email = s.email.or_else(|| cache.email.clone());
                    s.workspace_id = s.workspace_id.or_else(|| cache.workspace_id.clone());
                    s.plan = s.plan.or_else(|| cache.plan.clone());
                    s.credits = s.credits.or_else(|| cache.credits.clone());
                    s.refills = s.refills.or_else(|| cache.refills.clone());
                }
                providers.push(codex_view(&src.acc, &s, now));
            }
        }
        sort_by_order(&mut providers, &self.order);
        let revisions = self.accounts.iter().map(|a| (a.id.clone(), a.revision)).collect();
        Snapshot { now, claude: rabbit.map(|(_, st)| st), providers, logins: HashMap::new(), probes: HashMap::new(), revisions, claude_statuses }
    }
}

/// 공식 CLI가 로그아웃이라고 한 계정: 예전에 받아 둔 값은 지금 사용량이 아니므로 그래프·추가 사용·크레딧·리필권을 비움.
/// 위젯·메뉴 막대·설정 창·알림에서 모두 빠지고, 다시 로그인하면 새로 받은 값부터 표시.
/// (확인 실패처럼 상태를 모를 때는 그대로 둠)
pub fn hide_logged_out(snap: &mut Snapshot) {
    let out: Vec<String> = snap.providers.iter().filter(|p| snap.logged_out(&p.account_id)).map(|p| p.account_id.clone()).collect();
    for p in snap.providers.iter_mut().filter(|p| out.contains(&p.account_id)) {
        p.meters.clear();
        p.extra = None;
        p.credits = None;
        p.refills = None;
        p.coupons.clear();
        p.forecast = None;
    }
    snap.claude_statuses.retain(|id, _| !out.contains(id));
    // 토끼(예보·낮잠)는 위젯에 보이는 첫 Claude 계정을 따름: 그 계정이 로그아웃이면 예전 값으로 예보하지 않음
    let claude = || snap.providers.iter().filter(|p| p.id == ProviderId::Claude);
    let rabbit = claude().find(|p| p.show).or_else(|| claude().next()).map(|p| p.account_id.clone());
    if rabbit.is_some_and(|id| snap.logged_out(&id)) {
        snap.claude = None;
    }
}

/// 낮잠이 먼저 오는 예보 우선, 없으면 5시간 창의 "버텨요"
fn pick_forecast(session: Option<Forecast>, weekly: Option<Forecast>) -> Option<Forecast> {
    let naps: Vec<Forecast> = [session, weekly].into_iter().flatten().filter(|f| !f.safe).collect();
    naps.into_iter().min_by(|a, b| a.at.total_cmp(&b.at)).or(session)
}

// ------------------------------------------------------------------ 엔진

enum Cmd {
    Refresh,
    /// 다시 읽지 않고 스냅숏만 곧바로 다시 계산
    Poke,
    FilesChanged,
    UsageSaved(String),
    Stop,
}

enum ProbeCommand {
    Request(Option<String>),
    ResetTimes(HashMap<String, Vec<f64>>),
    Finished(String, Result<f64, UsageError>, Option<f64>, f64),
    Stop,
}

pub struct Engine {
    tx: Sender<Cmd>,
    cfg: Arc<Mutex<Config>>,
    login_tx: Sender<()>,
    /// Some(계정) = 그 계정만, None = 모든 계정을 지금 확인
    probe_tx: Sender<ProbeCommand>,
}

/// 바뀐 게 없어도 이 간격으로는 화면에 알림 (남은 시간 글자 갱신용)
const QUIET_SECS: f64 = 30.0;

/// 화면에 보이는 내용이 달라졌는지 비교할 요약. 매번 달라지는 시각(now, 쉰 시간)은 빼요.
#[derive(PartialEq)]
struct Shown {
    providers: Vec<ProviderView>,
    logins: HashMap<String, LoginStatus>,
    probes: HashMap<String, ProbeState>,
    rabbit: Option<String>,
    revisions: HashMap<String, u64>,
}

impl Shown {
    fn of(snap: &Snapshot) -> Self {
        let rabbit = snap.claude.as_ref().map(|s| {
            format!(
                "{:?}|{:.4}|{:.4}|{:?}|{:?}|{:?}|{:?}|{}|{:?}|{}|{}|{}",
                s.mood, s.session_pct, s.weekly_pct, s.session_reset_at, s.weekly_reset_at, s.nap_kind, s.wake_at, s.hop_speed, s.forecast, s.has_data, s.exact,
                s.long_idle()
            )
        });
        Shown { providers: snap.providers.clone(), logins: snap.logins.clone(), probes: snap.probes.clone(), rabbit, revisions: snap.revisions.clone() }
    }
}

impl Engine {
    /// 백그라운드 스레드 시작. on_snapshot은 다른 스레드에서 불리므로 UI 스레드로 넘기세요.
    pub fn start<F: Fn(Snapshot) + Send + 'static>(on_snapshot: F) -> Engine {
        // 첫 스냅숏·설정 파일 오류·새 설정 파일도 저장된 언어로 (컨트롤러보다 먼저 시작함)
        State::load().apply_locale();
        let cfg = Arc::new(Mutex::new(config::load()));
        let (tx, rx) = mpsc::channel();

        // 로그인 상태: 한 시간마다(또는 요청 시) 유효한 파일을 먼저 확인.
        // 주 폴더 로그인이 끊겼으면 같은 계정으로 로그인된 다른 폴더를 대신 씀 (fallback 모듈)
        let logins: Arc<Mutex<HashMap<String, LoginStatus>>> = Arc::new(Mutex::new(HashMap::new()));
        let active: Arc<Mutex<HashMap<String, PathBuf>>> = Arc::new(Mutex::new(HashMap::new()));
        let (login_tx, login_rx) = mpsc::channel::<()>();
        {
            let logins = logins.clone();
            let active = active.clone();
            let poke = tx.clone();
            std::thread::Builder::new()
                .name("usagi-login".into())
                .spawn(move || {
                    let mut pending_checks: HashMap<String, f64> = HashMap::new();
                    let mut regular_at = 0.0;
                    loop {
                        let list = accounts::load(&State::load());
                        let keys: HashSet<_> = list.iter().map(Account::usage_key).collect();
                        pending_checks.retain(|key, _| keys.contains(key));
                        let now = time::now();
                        let regular = regular_at <= now;
                        if regular {
                            regular_at = now + LOGIN_CHECK.as_secs_f64();
                        }
                        let mut moved = false;
                        let mut checked = false;
                        for acc in &list {
                            let key = acc.usage_key();
                            if !regular && pending_checks.get(&key).is_none_or(|at| *at > now) {
                                continue;
                            }
                            if pending_checks.contains_key(&key) && usage_retry::is_running(&key) {
                                pending_checks.insert(key, now + 1.0);
                                continue;
                            }
                            pending_checks.remove(&key);
                            let (st, dir, pending) = fallback::resolve(acc, &list);
                            checked = true;
                            if let Ok(mut g) = logins.lock() {
                                if pending.is_none() || g.get(&key).is_none_or(|old| old.logged_in != Some(true)) {
                                    g.insert(key.clone(), st);
                                }
                            }
                            if let Some(at) = pending {
                                pending_checks.insert(key, at);
                                continue;
                            }
                            if let Ok(mut g) = active.lock() {
                                let before = g.get(&key).cloned();
                                match &dir {
                                    Some(d) => g.insert(key, d.clone()),
                                    None => g.remove(&key),
                                };
                                moved |= before != dir;
                            }
                        }
                        if let Ok(mut g) = active.lock() {
                            g.retain(|key, _| keys.contains(key));
                        }
                        if let Ok(mut g) = logins.lock() {
                            g.retain(|key, _| keys.contains(key));
                        }
                        // 쓰는 폴더가 바뀌면 기록 읽기도 다시 구성
                        if checked {
                            let _ = poke.send(if moved { Cmd::Refresh } else { Cmd::Poke });
                        }
                        let next = pending_checks.values().copied().fold(regular_at, f64::min);
                        // clamp는 NaN을 그대로 두므로 max·min (NaN이면 다른 쪽 값)
                        let wait = time::duration_secs((next - time::now()).max(1.0).min(LOGIN_CHECK.as_secs_f64()));
                        match login_rx.recv_timeout(wait) {
                            Ok(()) => {
                                regular_at = 0.0;
                                while login_rx.try_recv().is_ok() {}
                            }
                            Err(RecvTimeoutError::Timeout) => {}
                            Err(RecvTimeoutError::Disconnected) => break,
                        }
                    }
                })
                .expect("spawn login thread");
        }

        // 사용량 확인: 갱신 주기마다 (쓰는 중이라 새 값이 따로 들어오면 덜 자주), 새로고침이면 즉시
        let probes: Arc<Mutex<HashMap<String, ProbeState>>> = Arc::new(Mutex::new(HashMap::new()));
        // Codex 실행 기록에서 마지막으로 새 값을 읽은 시각 (계정별, 기록을 읽는 쪽이 채움)
        let live: Arc<Mutex<HashMap<String, f64>>> = Arc::new(Mutex::new(HashMap::new()));
        let (probe_tx, probe_rx) = mpsc::channel::<ProbeCommand>();
        {
            let probes = probes.clone();
            let logins = logins.clone();
            let live = live.clone();
            let active = active.clone();
            let recheck = login_tx.clone();
            let poke = tx.clone();
            let completed = probe_tx.clone();
            std::thread::Builder::new()
                .name("usagi-probe".into())
                .spawn(move || {
                    let mut wait = Duration::from_secs(5); // 처음엔 로그인 상태 확인만 기다렸다가 바로
                    let started = time::now();
                    let initial_at = started + wait.as_secs_f64();
                    let mut reset_times = HashMap::new();
                    let mut checked_through = HashMap::new();
                    let mut auth_rechecks = HashMap::new();
                    let mut running = HashSet::new();
                    let mut forced = Vec::new();
                    let mut force_all = false;
                    loop {
                        let first = match probe_rx.recv_timeout(wait) {
                            Ok(command) => Some(command),
                            Err(RecvTimeoutError::Timeout) => None,
                            Err(RecvTimeoutError::Disconnected) => break,
                        };
                        for command in first.into_iter().chain(probe_rx.try_iter()) {
                            match command {
                                ProbeCommand::Stop => return,
                                ProbeCommand::Request(None) => force_all = true,
                                ProbeCommand::Request(Some(id)) if !forced.contains(&id) => forced.push(id),
                                ProbeCommand::Request(Some(_)) => {}
                                ProbeCommand::ResetTimes(times) => reset_times = times,
                                ProbeCommand::Finished(key, result, live_at, interval) => {
                                    running.remove(&key);
                                    let saved = result.is_ok();
                                    let logged_in = logins.lock().ok().and_then(|g| g.get(&key).and_then(|l| l.logged_in));
                                    if matches!(result, Err(UsageError::ProfileChanged | UsageError::IdentityChanged))
                                        || (result.is_ok() && logged_in != Some(true))
                                    {
                                        let _ = recheck.send(());
                                    } else if result.as_ref().err().is_some_and(UsageError::auth_recheck) {
                                        let last = auth_rechecks.entry(key.clone()).or_insert(0.0);
                                        if time::now() - *last > 600.0 {
                                            *last = time::now();
                                            let _ = recheck.send(());
                                        }
                                    }
                                    let now = time::now();
                                    checked_through.insert(key.clone(), result.as_ref().copied().unwrap_or(now));
                                    let retry = usage_retry::deadline(&key).filter(|at| *at > now)
                                        .or_else(|| matches!(result, Err(UsageError::Busy)).then_some(now + 1.0));
                                    if let Ok(mut states) = probes.lock() {
                                        if let Some(probe) = states.get_mut(&key) {
                                            probe.finished(result.map(|_| ()), now, retry);
                                            probe.next_at = probe.tried_at.map(|at| next_usage_at(at, live_at, interval));
                                        }
                                    }
                                    let _ = poke.send(if saved { Cmd::UsageSaved(key) } else { Cmd::Poke });
                                }
                            }
                        }
                        let mut next_retry = f64::INFINITY;
                        let state = State::load();
                        let interval = refresh_minutes(&state);
                        let mut list = accounts::load(&state);
                        if std::mem::take(&mut force_all) {
                            for account in &list {
                                if !forced.contains(&account.id) { forced.push(account.id.clone()); }
                            }
                        }
                        forced.retain(|id| list.iter().any(|account| &account.id == id));
                        let keys: HashSet<_> = list.iter().map(Account::usage_key).collect();
                        auth_rechecks.retain(|key, _| keys.contains(key));
                        checked_through.retain(|key, _| keys.contains(key));
                        if let Ok(mut g) = probes.lock() {
                            g.retain(|key, _| keys.contains(key));
                        }
                        list.sort_by_key(|a| forced.iter().position(|id| id == &a.id).unwrap_or(usize::MAX));
                        for primary in list {
                            let key = primary.usage_key();
                            if running.len() >= 3 { break; }
                            let force = forced.iter().position(|id| *id == primary.id)
                                .map(|i| forced.remove(i)).is_some();
                            if running.contains(&key) { continue; }
                            // 주 폴더 로그인이 끊겨 다른 폴더를 쓰는 중이면 그 폴더로 조회 (id는 그대로)
                            let fallback = active.lock().ok().and_then(|g| g.get(&key).cloned())
                                .filter(|dir| accounts::fallback_profile_available(dir))
                                .map(|dir| fallback::at(&primary, &dir));
                            let acc = fallback.as_ref().unwrap_or(&primary);
                            let login = logins.lock().ok().and_then(|g| g.get(&key).cloned());
                            let logged_out = login.as_ref().and_then(|status| status.logged_in) == Some(false);
                            let retry_at = usage_retry::deadline(&key).or_else(|| {
                                probes.lock().ok().and_then(|g| g.get(&key)?.retry_at)
                            });
                            if !force && logged_out && retry_at.is_none()
                                && !crate::login::has_file_session(&acc.provider, &acc.path())
                            {
                                continue;
                            }
                            let now = time::now();
                            if !force && now < initial_at {
                                next_retry = next_retry.min(initial_at);
                                continue;
                            }
                            if let Some(at) = retry_at.filter(|at| *at > now) {
                                next_retry = next_retry.min(at);
                                let changed = probes.lock().ok().is_some_and(|mut g| {
                                    let p = g.entry(key.clone()).or_default();
                                    if p.retry_at == Some(at) { return false; }
                                    p.retry_at = Some(at);
                                    p.error = Some(UsageError::Deferred.to_string());
                                    true
                                });
                                if changed {
                                    let _ = poke.send(Cmd::Poke);
                                }
                                continue;
                            }
                            let tried = probes.lock().ok().and_then(|g| g.get(&key).and_then(|p| p.tried_at));
                            let live_at = match acc.provider.as_str() {
                                // 파일 로그인이나 /usage로 읽음. 상태 표시줄에서 새 값이 오면 덜 자주 확인.
                                "claude" if usage_cmd::available(acc) => {
                                    bridge::read(&key).map(|b| b.updated_at)
                                }
                                // Codex 읽기도 토큰 0. Codex를 쓰는 중이면 실행 기록에 값이 남으므로 덜 자주
                                "codex" => live.lock().ok().and_then(|g| g.get(&key).copied()),
                                _ if force || retry_at.is_some() => None,
                                _ => continue,
                            };
                            let mode = if !force && retry_at.is_none() {
                                let regular_at = tried.map_or(now, |tried| next_usage_at(tried, live_at, interval));
                                let (at, mode) = scheduled_usage_at(
                                    regular_at,
                                    reset_times.get(&key).map_or(&[], Vec::as_slice),
                                    checked_through.get(&key).copied().unwrap_or(started),
                                    now,
                                );
                                if at > now {
                                    next_retry = next_retry.min(at);
                                    let changed = probes.lock().ok().is_some_and(|mut g| {
                                        let p = g.entry(key.clone()).or_default();
                                        if p.next_at == Some(at) { return false; }
                                        p.next_at = Some(at);
                                        true
                                    });
                                    if changed {
                                        let _ = poke.send(Cmd::Poke);
                                    }
                                    continue;
                                }
                                mode
                            } else {
                                UsageMode::WithCli
                            };
                            if let Ok(mut g) = probes.lock() {
                                g.entry(key.clone()).or_default().running = true;
                            }
                            let _ = poke.send(Cmd::Poke);
                            running.insert(key.clone());
                            let send = completed.clone();
                            let worker_key = key.clone();
                            if let Err(error) = std::thread::Builder::new().name("usagi-usage".into()).spawn(move || {
                                let target = fallback.as_ref().unwrap_or(&primary);
                                let started_at = time::now();
                                let result = if target.is_claude() {
                                    fallback::claude_usage(&primary, target, login.as_ref(), mode)
                                        .and_then(|file| {
                                            let through = checked_quota_through(started_at, time::now(), claude_reset_times(&file));
                                            primary.commit_usage(|| bridge::save(&worker_key, file)).map(|()| through)
                                        })
                                } else {
                                    fallback::codex_usage(&primary, target, mode)
                                        .and_then(|snapshot| {
                                            let through = checked_quota_through(started_at, time::now(), codex_reset_times(&snapshot));
                                            primary.commit_usage(|| codex::save(&worker_key, &snapshot)).map(|()| through)
                                        })
                                };
                                let _ = send.send(ProbeCommand::Finished(worker_key, result, live_at, interval));
                            }) {
                                let _ = completed.send(ProbeCommand::Finished(key, Err(UsageError::Failed(error.to_string())), live_at, interval));
                            }
                        }
                        wait = time::duration_secs((next_retry - time::now()).max(0.0).min(30.0));
                    }
                })
                .expect("spawn probe thread");
        }

        let shared = cfg.clone();
        let log_wake = tx.clone();
        let schedule_tx = probe_tx.clone();
        std::thread::Builder::new()
            .name("usagi-poll".into())
            .spawn(move || {
                let mut c = shared.lock().map(|g| g.clone()).unwrap_or_default();
                let state = State::load();
                config::apply_prefs(&mut c, &state);
                let active_now = || active.lock().map(|g| g.clone()).unwrap_or_default();
                let mut sources = Sources::build(&c, &accounts::load(&state), &active_now());
                let mut watch = LogWatch::new(move || { let _ = log_wake.send(Cmd::FilesChanged); });
                let mut reconcile_at = Instant::now();
                let mut reset_times = HashMap::new();
                let mut schedule_dirty = true;
                // 바뀐 게 없으면 화면 쪽(위젯·설정 창)을 깨우지 않음. 요청(새로고침·확인 끝)이 오면 바로 보냄.
                let mut last: Option<Shown> = None;
                let mut last_sent = 0.0;
                let mut force = true;
                loop {
                    if Instant::now() >= reconcile_at {
                        // A backend may silently drop a watch after an I/O error.
                        let healthy = watch.sync(sources.watch_interests(), true);
                        sources.rescan();
                        schedule_dirty = true;
                        reconcile_at = Instant::now() + if healthy { log_watch::RECONCILE } else { log_watch::RETRY_WATCH };
                    }
                    let mut snap = sources.snapshot(&c);
                    if let Ok(mut g) = live.lock() {
                        g.retain(|key, _| sources.codex.iter().any(|src| src.acc.usage_key() == *key));
                        for src in &sources.codex {
                            if let Some(s) = &src.reader.snapshot {
                                g.insert(src.acc.usage_key(), s.ts);
                            }
                        }
                    }
                    if std::mem::take(&mut schedule_dirty) {
                        let updated = sources.reset_times();
                        if updated != reset_times {
                            reset_times = updated;
                            let _ = schedule_tx.send(ProbeCommand::ResetTimes(reset_times.clone()));
                        }
                    }
                    if let Ok(g) = logins.lock() {
                        for acc in &sources.accounts {
                            if let Some(login) = g.get(&acc.usage_key()) {
                                snap.logins.insert(acc.id.clone(), login.clone());
                            }
                        }
                    }
                    hide_logged_out(&mut snap);
                    if let Ok(g) = probes.lock() {
                        for acc in &sources.accounts {
                            if let Some(probe) = g.get(&acc.usage_key()) {
                                snap.probes.insert(acc.id.clone(), probe.clone());
                            }
                        }
                    }
                    let shown = Shown::of(&snap);
                    if force || last.as_ref() != Some(&shown) || snap.now - last_sent >= QUIET_SECS {
                        last = Some(shown);
                        last_sent = snap.now;
                        on_snapshot(snap);
                    }
                    force = false;
                    let wait = time::duration_secs(c.poll_seconds)
                        .min(reconcile_at.saturating_duration_since(Instant::now()));
                    match rx.recv_timeout(wait) {
                        Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                        Ok(Cmd::Poke) => force = true,
                        Ok(Cmd::UsageSaved(key)) => {
                            sources.usage_saved(&key);
                            schedule_dirty = true;
                            force = true;
                        }
                        Ok(Cmd::FilesChanged) => {
                            // A single write can produce several native notifications.
                            std::thread::sleep(Duration::from_millis(80));
                            let changes = watch.take();
                            if changes.rescan || !changes.directories.is_empty() {
                                let healthy = watch.sync(sources.watch_interests(), changes.rescan);
                                if !healthy {
                                    reconcile_at = reconcile_at.min(Instant::now() + log_watch::RETRY_WATCH);
                                } else if changes.rescan {
                                    reconcile_at = Instant::now() + log_watch::RECONCILE;
                                }
                            }
                            sources.read_changes(&changes);
                            schedule_dirty = true;
                        }
                        Ok(Cmd::Refresh) => {
                            force = true;
                            c = config::load();
                            let state = State::load();
                            config::apply_prefs(&mut c, &state);
                            if let Ok(mut g) = shared.lock() {
                                *g = c.clone();
                            }
                            sources.reconfigure(&c, &accounts::load(&state), &active_now());
                            reconcile_at = Instant::now();
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                }
            })
            .expect("spawn poll thread");
        Engine { tx, cfg, login_tx, probe_tx }
    }

    /// 사용량을 지금 확인 (None = 모든 계정)
    pub fn probe_now(&self, account_id: Option<String>) {
        let _ = self.probe_tx.send(ProbeCommand::Request(account_id));
    }

    /// 로그인 상태를 곧바로 다시 확인
    pub fn check_logins(&self) {
        let _ = self.login_tx.send(());
    }

    /// 설정/계정을 다시 읽고 곧바로 갱신
    pub fn refresh(&self) {
        let _ = self.tx.send(Cmd::Refresh);
    }

    pub fn stop(&self) {
        let _ = self.probe_tx.send(ProbeCommand::Stop);
        let _ = self.tx.send(Cmd::Stop);
    }

    pub fn config(&self) -> Config {
        self.cfg.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// 테스트용: 스레드 없이 명령을 버리는 엔진 (CLI를 실행하지 않음)
    #[cfg(any(test, feature = "mock"))]
    pub(crate) fn idle() -> Engine {
        let (tx, _) = mpsc::channel();
        let (login_tx, _) = mpsc::channel();
        let (probe_tx, _) = mpsc::channel();
        Engine { tx, cfg: Arc::new(Mutex::new(Config::default())), login_tx, probe_tx }
    }

    /// 테스트 빌드(mock): CLI·네트워크·로그 읽기 없이, 등록된 계정마다 가짜 사용량을 3초마다 보냄.
    /// 화면(위젯·메뉴 막대·설정 창) 동작을 실제 계정과 상관없이 확인하는 용도
    #[cfg(feature = "mock")]
    pub fn mock<F: Fn(Snapshot) + Send + 'static>(on_snapshot: F) -> Engine {
        std::thread::Builder::new()
            .name("usagi-mock".into())
            .spawn(move || loop {
                on_snapshot(mock_snapshot(time::now()));
                std::thread::sleep(Duration::from_secs(3));
            })
            .expect("spawn mock thread");
        Engine::idle()
    }
}

/// 가짜 사용량: 계정 순서대로 조금씩 다른 값이 천천히 오르내림 (막대가 움직이는지도 보이게)
#[cfg(feature = "mock")]
fn mock_snapshot(now: f64) -> Snapshot {
    let accounts = accounts::load(&State::load());
    let wave = |base: f64, phase: f64| (base + 0.08 * (now / 40.0 + phase).sin()).clamp(0.0, 1.0);
    let mut providers = Vec::new();
    let mut logins = HashMap::new();
    for (i, acc) in accounts.iter().enumerate() {
        let p = i as f64;
        let meter = |label: &str, ratio: f64, resets_in: f64| Meter { label: label.into(), ratio, resets_at: Some(now + resets_in), exact: true };
        let (id, name, meters) = if acc.is_claude() {
            (ProviderId::Claude, "Claude", vec![meter("5시간", wave(0.62, p), 2.2 * HOUR), meter("주간", wave(0.38, p + 1.0), 4.0 * DAY)])
        } else {
            (ProviderId::Codex, "Codex", vec![meter("5시간", wave(0.47, p), 3.1 * HOUR), meter("주간", wave(0.12, p + 2.0), 5.5 * DAY)])
        };
        providers.push(ProviderView {
            id,
            account_id: acc.id.clone(),
            name: name.into(),
            meters,
            updated_at: Some(now),
            note: None,
            forecast: None,
            show: acc.show,
            email: None,
            workspace_id: None,
            plan: None,
            credits: None,
            // Claude 카드의 리필권·쿠폰 표시 확인용
            refills: acc.is_claude().then(|| codex::Refills { available: 2, expires_at: Some(now + 6.0 * DAY) }),
            extra: None,
            coupons: if acc.is_claude() {
                vec![bridge::Coupon { amount: 50.0, currency: "USD".into(), expires_at: now + 3.0 * DAY + 5.0 * HOUR, name: None }]
            } else {
                Vec::new()
            },
        });
        logins.insert(acc.id.clone(), LoginStatus { logged_in: Some(true), checked_at: now, ..Default::default() });
    }
    let revisions = accounts.iter().map(|a| (a.id.clone(), a.revision)).collect();
    Snapshot { now, claude: None, providers, logins, probes: HashMap::new(), revisions, claude_statuses: HashMap::new() }
}

/// 한 번만 계산 (CLI `--status` 등)
pub fn snapshot_once() -> Snapshot {
    let state = State::load();
    state.apply_locale();
    let c = config::load();
    let mut sources = Sources::build(&c, &accounts::load(&state), &HashMap::new());
    sources.rescan();
    sources.snapshot(&c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::BridgeWindow;
    use crate::tracker::Mood;

    #[test]
    fn probe_scheduling_respects_closed_window() {
        let now = 1_000_000.0;
        let open = BridgeFile {
            updated_at: now - 400.0,
            five_hour: Some(BridgeWindow { used_percentage: 10.0, resets_at: Some(now + 3600.0) }),
            seven_day: None,
            weekly_anchor: None,
            ..Default::default()
        };
        let closed = BridgeFile { five_hour: Some(BridgeWindow { used_percentage: 10.0, resets_at: Some(now - 10.0) }), ..open.clone() };
        assert!(probe_due(None, None, 5.0, now)); // 처음 한 번
        assert!(!probe_due(None, Some(now - 60.0), 5.0, now)); // 실패 후 10분 안엔 다시 안 보냄
        assert!(!probe_due(Some(&closed), None, 1.0, now)); // 창이 닫혀 있으면 자동으로 안 보냄
        assert!(probe_due(Some(&open), Some(now - 400.0), 5.0, now)); // 5분 지남
        assert!(!probe_due(Some(&open), Some(now - 100.0), 5.0, now));
        assert!(!probe_due(Some(&open), Some(now - 400.0), 10.0, now));
    }

    #[test]
    fn usage_checks_less_often_while_live_values_arrive() {
        let now = 1_000_000.0;
        let min = 60.0;
        assert!(usage_due(None, None, 10.0, now), "처음 한 번");
        assert!(!usage_due(Some(now - 5.0 * min), None, 10.0, now));
        assert!(usage_due(Some(now - 10.0 * min), None, 10.0, now), "주기 지남");
        // 마지막 확인 뒤 상태 표시줄로 새 값이 들어옴 → 30분까지 미룸
        let tried = now - 12.0 * min;
        assert!(!usage_due(Some(tried), Some(now - 1.0 * min), 10.0, now));
        assert!(usage_due(Some(now - 31.0 * min), Some(now - 1.0 * min), 10.0, now));
        // 들어온 값이 오래됐으면 원래 주기
        assert!(usage_due(Some(tried), Some(now - 11.0 * min), 10.0, now));
        // 우리 확인이 남긴 값은 쓰는 중 값이 아님
        assert!(usage_due(Some(tried), Some(tried + 2.0), 10.0, now));
    }

    #[test]
    fn reset_check_is_once_per_boundary_and_keeps_regular_checks() {
        let regular = 600.0;
        let resets = [100.0, 100.0, 300.0];
        assert!(matches!(scheduled_usage_at(regular, &resets, 70.0, 90.0), (100.0, UsageMode::ApiOnly)));
        assert!(matches!(scheduled_usage_at(regular, &resets, 70.0, 100.0), (100.0, UsageMode::ApiOnly)));
        assert!(matches!(scheduled_usage_at(regular, &resets, 102.0, 102.0), (300.0, UsageMode::ApiOnly)));
        assert!(matches!(scheduled_usage_at(regular, &[900.0], 70.0, 100.0), (600.0, UsageMode::WithCli)));
        assert!(matches!(scheduled_usage_at(regular, &resets, 301.0, 301.0), (600.0, UsageMode::WithCli)));
        assert!(matches!(scheduled_usage_at(regular, &resets, 70.0, 600.0), (600.0, UsageMode::WithCli)));
    }

    #[test]
    fn older_quota_from_a_request_spanning_reset_leaves_the_reset_due() {
        let through = checked_quota_through(98.0, 102.0, [100.0, 300.0].into_iter());
        assert!(matches!(scheduled_usage_at(600.0, &[100.0, 300.0], through, 102.0), (100.0, UsageMode::ApiOnly)));
        assert_eq!(checked_quota_through(98.0, 102.0, [200.0, 300.0].into_iter()), 102.0);
        assert_eq!(checked_quota_through(101.0, 102.0, [100.0, 300.0].into_iter()), 102.0);
    }

    #[test]
    fn unavailable_reset_api_preserves_the_normal_check_and_its_status() {
        let mut probe = ProbeState {
            running: true,
            tried_at: Some(90.0),
            error: Some("previous error".into()),
            ..Default::default()
        };
        probe.finished(Err(UsageError::NoApiSession), 100.0, None);
        assert_eq!(probe.tried_at, Some(90.0));
        assert_eq!(probe.error.as_deref(), Some("previous error"));
        assert!(!probe.running);
    }

    #[test]
    fn alert_rules_and_once_per_window() {
        let mut n = Notices { state: State::default() };
        n.state.0.insert("alerts".into(), serde_json::json!({ "claude-1:5시간": { "on": true, "at": [50, 80] } }));
        assert_eq!(alert_rule(&n.state, "claude-1:5시간"), (true, vec![50.0, 80.0]));
        assert_eq!(alert_rule(&n.state, "claude-1:주간"), (true, vec![]), "기본은 알림 없음");
        let now = time::now();
        let mk = |ratio: f64| Snapshot {
            now,
            claude: None,
            providers: vec![ProviderView {
                id: ProviderId::Claude,
                account_id: "claude-1".into(),
                name: "Claude".into(),
                meters: vec![Meter { label: "5시간".into(), ratio, resets_at: Some(now + 3600.0), exact: true }],
                updated_at: Some(now),
                note: None,
                forecast: None,
                show: true,
                email: None,
                workspace_id: None,
                plan: None,
                credits: None,
                refills: None,
                extra: None,
                coupons: Vec::new(),
            }],
            logins: HashMap::new(),
            probes: HashMap::new(),
            claude_statuses: HashMap::new(), revisions: HashMap::new(),
        };
        let cfg = Config::default();
        assert_eq!(n.check(None, &mk(0.4), &cfg).len(), 0);
        assert_eq!(n.check(None, &mk(0.55), &cfg).len(), 1); // 50%
        assert_eq!(n.check(None, &mk(0.6), &cfg).len(), 0); // 같은 기준 다시 안 알림
        assert_eq!(n.check(None, &mk(0.85), &cfg).len(), 1); // 80%
        assert_eq!(n.check(None, &mk(0.9), &cfg).len(), 0);
    }

    #[test]
    fn coupon_expiry_alerts_follow_the_refill_setting_once_per_expiry() {
        let mut n = Notices { state: State::default() };
        let now = time::now();
        let mk = |expires_at: f64| Snapshot {
            now,
            claude: None,
            providers: vec![ProviderView {
                id: ProviderId::Claude,
                account_id: "claude-1".into(),
                name: "Claude".into(),
                meters: Vec::new(),
                updated_at: Some(now),
                note: None,
                forecast: None,
                show: true,
                email: None,
                workspace_id: None,
                plan: None,
                credits: None,
                refills: None,
                extra: None,
                coupons: vec![bridge::Coupon { amount: 50.0, currency: "USD".into(), expires_at, name: None }],
            }],
            logins: HashMap::new(),
            probes: HashMap::new(),
            claude_statuses: HashMap::new(), revisions: HashMap::new(),
        };
        let cfg = Config::default();
        let soon = mk(now + 2.0 * HOUR);
        assert!(n.check(None, &soon, &cfg).is_empty(), "기본은 리필권·쿠폰 알림 꺼짐");
        n.state.0.insert("prefs".into(), serde_json::json!({ "refillNotify": true }));
        let sent = n.check(None, &soon, &cfg);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].0, "쿠폰 만료 임박");
        assert!(sent[0].1.starts_with("Claude 쿠폰 $50 · "), "{}", sent[0].1);
        assert!(n.check(None, &soon, &cfg).is_empty(), "같은 만료는 한 번만");
        assert!(n.check(None, &mk(now + 3.0 * DAY), &cfg).is_empty(), "기본 시점(하루 전)보다 이름");
        n.state.0.insert("refill_rules".into(), serde_json::json!({ "claude-1": { "on": false } }));
        assert!(n.check(None, &mk(now + HOUR), &cfg).is_empty(), "계정별 끔");
    }

    #[test]
    fn alert_set_reads_old_and_new_shapes() {
        let mut st = State::default();
        st.0.insert(
            "alerts".into(),
            serde_json::json!({
                "a:5시간": { "on": false, "at": 80 },
                "a:주간": { "at": [80, 50, 0, 100, 50], "at_off": [80, 30], "before": [30, 10, 10, 20000], "before_off": [10, 60] },
            }),
        );
        // 예전 형식 (on/at만, at이 숫자 하나)
        let old = alert_set(&st, "a:5시간");
        assert_eq!((old.on, old.at.clone()), (false, vec![80.0]));
        assert!(old.at_off.is_empty() && old.before.is_empty() && old.before_off.is_empty());
        assert_eq!(alert_rule(&st, "a:5시간"), (false, vec![80.0]));
        // 새 형식: 범위 밖 값은 버리고, 켜짐·꺼짐 양쪽에 있으면 켜짐
        let s = alert_set(&st, "a:주간");
        assert_eq!(
            s,
            AlertSet { on: true, at: vec![50.0, 80.0], at_off: vec![30.0], before: vec![10.0, 30.0], before_off: vec![60.0] }
        );
        assert_eq!(alert_rule(&st, "a:주간"), (true, vec![50.0, 80.0]));
        // 설정 없음: 켜짐, 기준 없음
        let none = alert_set(&st, "a:없음");
        assert!(none.on && none.at.is_empty() && none.before.is_empty());
    }

    fn reset_snap(now: f64, resets_at: f64, ratio: f64) -> Snapshot {
        Snapshot {
            now,
            claude: None,
            providers: vec![ProviderView {
                id: ProviderId::Claude,
                account_id: "claude-1".into(),
                name: "Claude".into(),
                meters: vec![Meter { label: "5시간".into(), ratio, resets_at: Some(resets_at), exact: true }],
                updated_at: Some(now),
                note: None,
                forecast: None,
                show: true,
                email: None,
                workspace_id: None,
                plan: None,
                credits: None,
                refills: None,
                extra: None,
                coupons: Vec::new(),
            }],
            logins: HashMap::new(),
            probes: HashMap::new(),
            claude_statuses: HashMap::new(), revisions: HashMap::new(),
        }
    }

    #[test]
    fn reset_soon_fires_once_per_window_with_the_smallest_lead() {
        let mut n = Notices { state: State::default() };
        n.state.0.insert("alerts".into(), serde_json::json!({ "claude-1:5시간": { "before": [10, 30, 60, 360] } }));
        let cfg = Config::default();
        let reset = time::now() + 3.0 * HOUR;
        let at = |left_min: f64| reset_snap(reset - left_min * 60.0, reset, 0.3);
        assert!(n.check(None, &at(120.0), &cfg).is_empty(), "아직 어느 시점에도 안 맞음");
        let first = n.check(None, &at(50.0), &cfg);
        assert_eq!(first.len(), 1, "60분 전 기준");
        assert_eq!(first[0].0, "리셋 임박");
        assert!(first[0].1.starts_with("Claude 5시간 한도 50분 뒤 리셋 ("), "{}", first[0].1);
        assert!(n.check(None, &at(40.0), &cfg).is_empty(), "같은 창에서 다시 안 알림");
        assert_eq!(n.check(None, &at(25.0), &cfg).len(), 1, "30분 전 기준");
        // 창 안에서 다시 시작해도 이미 보낸 시점은 생략
        let mut again = Notices { state: n.state.clone() };
        assert!(again.check(None, &at(20.0), &cfg).is_empty());
        assert_eq!(again.check(None, &at(5.0), &cfg).len(), 1, "10분 전 기준");
        assert!(again.check(None, &at(4.0), &cfg).is_empty());
        // 창 중간에 처음 보면 맞는 시점 중 가장 작은 것 하나만 (더 큰 시점은 보낸 것으로 봄)
        let mut late = Notices { state: State::default() };
        late.state.0.insert("alerts".into(), n.state.0["alerts"].clone());
        assert_eq!(late.check(None, &at(5.0), &cfg).len(), 1);
        assert!(late.check(None, &at(3.0), &cfg).is_empty());
        // 다음 창은 다시 알림
        let next = reset + 5.0 * HOUR;
        assert_eq!(late.check(None, &reset_snap(next - 300.0, next, 0.3), &cfg).len(), 1);
    }

    #[test]
    fn reset_soon_respects_graph_item_and_global_switches() {
        let reset = time::now() + HOUR;
        let snap = reset_snap(reset - 300.0, reset, 0.3);
        let quiet = |rule: serde_json::Value, cfg: &Config| {
            let mut n = Notices { state: State::default() };
            n.state.0.insert("alerts".into(), serde_json::json!({ "claude-1:5시간": rule }));
            n.check(None, &snap, cfg).is_empty()
        };
        let cfg = Config::default();
        assert!(!quiet(serde_json::json!({ "before": [10] }), &cfg), "켜 둔 시점은 알림");
        assert!(quiet(serde_json::json!({ "on": false, "before": [10] }), &cfg), "그래프 알림 꺼짐");
        assert!(quiet(serde_json::json!({ "before_off": [10, 30] }), &cfg), "꺼 둔 시점");
        assert!(quiet(serde_json::json!({ "before": [10] }), &Config { notify: false, ..Config::default() }), "전체 알림 꺼짐");
        // 쓴 양이 없으면 리셋 알림 없음
        let mut n = Notices { state: State::default() };
        n.state.0.insert("alerts".into(), serde_json::json!({ "claude-1:5시간": { "before": [10] } }));
        assert!(n.check(None, &reset_snap(reset - 300.0, reset, 0.0), &cfg).is_empty());
    }

    #[test]
    fn hiding_the_widget_does_not_disable_usage_or_reset_alerts() {
        let now = time::now();
        let mut snap = reset_snap(now, now + 300.0, 0.8);
        snap.providers[0].show = false;
        let mut notices = Notices { state: State::default() };
        notices.state.0.insert("alerts".into(), serde_json::json!({ "claude-1:5시간": { "at": [50], "before": [10] } }));
        assert_eq!(notices.check(None, &snap, &Config::default()).len(), 2);
    }

    #[test]
    fn late_snapshot_from_a_previous_connection_is_ignored() {
        let now = time::now();
        let mut snap = reset_snap(now, now + 300.0, 0.8);
        let account: Account = serde_json::from_value(serde_json::json!({
            "id": "claude-1", "provider": "claude", "dir": "profile", "revision": 2
        })).unwrap();
        snap.revisions.insert(account.id.clone(), 1);
        snap.logins.insert(account.id.clone(), LoginStatus { logged_in: Some(true), ..Default::default() });
        snap.probes.insert(account.id.clone(), ProbeState { tried_at: Some(now), ..Default::default() });
        snap.retain_current(&[account]);
        assert!(snap.providers.is_empty());
        assert!(snap.logins.is_empty());
        assert!(snap.probes.is_empty());
    }

    #[test]
    fn login_expiry_notice_only_when_turned_on() {
        let mut n = Notices { state: State::default() };
        let mut snap = Snapshot { now: time::now(), claude: None, providers: vec![], logins: HashMap::new(), probes: HashMap::new(), claude_statuses: HashMap::new(), revisions: HashMap::new() };
        snap.logins.insert("claude-1".into(), LoginStatus { logged_in: Some(false), ..Default::default() });
        let cfg = Config::default();
        n.state.set_pref("loginNotify", serde_json::json!(false));
        assert!(n.check(None, &snap, &cfg).is_empty(), "설정에서 껐으면 로그인 만료 알림 없음");
        n.state.set_pref("loginNotify", serde_json::json!(true));
        assert_eq!(n.check(None, &snap, &cfg).len(), 1);
        assert!(n.check(None, &snap, &cfg).is_empty(), "한 번만");
    }

    #[test]
    fn login_warning_survives_restart_and_rearms_after_reconnect() {
        let expiry = time::parse_iso("2026-12-20T12:00:00Z").unwrap();
        let mut snap = reset_snap(expiry - 4.0 * DAY, expiry, 0.0);
        snap.revisions.insert("claude-1".into(), 1);
        snap.logins.insert("claude-1".into(), crate::login::parse_claude(
            "Sign-in ends: 2026-12-20T12:00:00Z",
        ));
        let cfg = Config::default();
        let mut notices = Notices { state: State::default() };
        assert!(notices.check(None, &snap, &cfg).is_empty());

        snap.now = expiry - 3.0 * DAY;
        assert_eq!(notices.check(None, &snap, &cfg).len(), 1);
        let mut notices = Notices::load();
        assert!(notices.check(None, &snap, &cfg).is_empty());

        snap.now = expiry + DAY;
        snap.logins.insert("claude-1".into(), LoginStatus { logged_in: Some(false), ..Default::default() });
        assert!(notices.check(None, &snap, &cfg).is_empty(), "no second alert on expiry");

        snap.revisions.insert("claude-1".into(), 2);
        snap.now = time::parse_iso("2027-01-18T12:00:00Z").unwrap();
        snap.logins.insert("claude-1".into(), crate::login::parse_claude(
            "Sign-in ends: 2027-01-20T12:00:00Z",
        ));
        assert_eq!(notices.check(None, &snap, &cfg).len(), 1);
    }

    #[test]
    fn logged_out_accounts_show_no_old_values() {
        let now = time::now();
        let view = |id: &str| ProviderView {
            id: ProviderId::Claude,
            account_id: id.into(),
            name: "Claude".into(),
            meters: vec![Meter { label: "5시간".into(), ratio: 0.55, resets_at: Some(now + 3600.0), exact: true }],
            updated_at: Some(now - 10.0 * HOUR),
            note: None,
            forecast: None,
            show: true,
            email: None,
            workspace_id: None,
            plan: None,
            credits: None,
            refills: None,
            extra: Some("추가 사용 $12.30 / $50 · 월".into()),
            coupons: Vec::new(),
        };
        let st = Tracker::new().status(now);
        let mut snap = Snapshot { now, claude: Some(st.clone()), providers: vec![view("claude-1"), view("claude-2"), view("claude-3")], logins: HashMap::new(), probes: HashMap::new(), claude_statuses: HashMap::new(), revisions: HashMap::new() };
        snap.logins.insert("claude-1".into(), LoginStatus { logged_in: Some(false), ..Default::default() });
        snap.logins.insert("claude-2".into(), LoginStatus { logged_in: Some(true), ..Default::default() });
        // claude-3: 확인 실패 (상태 모름)
        snap.logins.insert("claude-3".into(), LoginStatus { logged_in: None, error: Some("timeout".into()), ..Default::default() });
        hide_logged_out(&mut snap);
        assert!(snap.providers[0].meters.is_empty(), "로그아웃: 예전 값 숨김");
        assert!(snap.providers[0].extra.is_none(), "로그아웃: 예전 추가 사용 금액도 숨김");
        assert_eq!(snap.providers[1].meters.len(), 1, "로그인됨: 그대로");
        assert!(snap.providers[1].extra.is_some());
        assert_eq!(snap.providers[2].meters.len(), 1, "상태 모름: 그대로");
        assert!(snap.claude.is_none(), "토끼가 따르는 첫 Claude 계정이 로그아웃: 예보 없음");
        // 첫 계정을 위젯에서 숨기면 토끼는 다음 계정(로그인됨)을 따르므로 그대로
        let mut snap2 = Snapshot { now, claude: Some(st), providers: vec![view("claude-1"), view("claude-2")], logins: snap.logins.clone(), probes: HashMap::new(), claude_statuses: HashMap::new(), revisions: HashMap::new() };
        snap2.providers[0].show = false;
        hide_logged_out(&mut snap2);
        assert!(snap2.claude.is_some());
        // 숨긴 계정은 사용량 알림도 없음
        let mut n = Notices { state: State::default() };
        n.state.set_pref("loginNotify", serde_json::json!(false));
        n.state.0.insert("alerts".into(), serde_json::json!({ "claude-1:5시간": { "at": [50] } }));
        assert!(n.check(None, &snap, &Config::default()).is_empty());
    }

    #[test]
    fn claude_view_hides_values_without_exact() {
        let t = Tracker::new();
        let st = t.status(time::now());
        let acc = Account { id: "claude-1".into(), provider: "claude".into(), dir: ".".into(), label: "회사".into(), show: true, chain: None, revision: 0, statusline_original: None, identity: None };
        let v = claude_view(&acc, &st, None);
        assert_eq!(v.name, "Claude (회사)");
        assert!(v.meters.iter().all(|m| !m.exact));
        assert!(v.forecast.is_none());
        assert_eq!(st.mood, Mood::Rest);
    }

    #[test]
    fn claude_view_keeps_missing_weekly_quota_unknown() {
        let mut st = Tracker::new().status(1_000_000.0);
        let file = BridgeFile {
            five_hour: Some(BridgeWindow { used_percentage: 12.0, resets_at: None }),
            weekly_unavailable: true,
            weekly_anchor: Some(st.now + HOUR),
            ..Default::default()
        };
        apply_exact(&mut st, &file, 0.8);
        let acc = Account { id: "claude-1".into(), provider: "claude".into(), dir: ".".into(), label: String::new(), show: true, chain: None, revision: 0, statusline_original: None, identity: None };
        let view = claude_view(&acc, &st, Some(&file));
        assert!(view.meters[0].exact);
        assert!(!view.meters[1].exact);
        assert!(view.meters[1].resets_at.is_none());
    }

    #[test]
    fn cooldown_keeps_the_time_of_the_last_actual_request() {
        let mut probe = ProbeState { running: true, ..ProbeState::default() };
        probe.finished(Err(UsageError::RateLimited(120.0)), 100.0, Some(120.0));
        probe.finished(Err(UsageError::Deferred), 110.0, Some(120.0));
        assert_eq!(probe.tried_at, Some(100.0));
        assert_eq!(probe.retry_at, Some(120.0));
        probe.finished(Ok(()), 121.0, None);
        assert_eq!(probe.tried_at, Some(121.0));
        assert!(probe.retry_at.is_none());
        assert!(probe.error.is_none());
    }
}
