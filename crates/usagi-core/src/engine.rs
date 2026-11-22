//! 백그라운드 엔진: 정확한 값 받기 → 공급자별 미터 → 토끼 상태 → 알림.
//!
//! 추정은 하지 않습니다. 화면에 나오는 숫자는 모두 서버가 준 값입니다.
//! - Claude: 공식 CLI의 아주 작은 확인 요청(rate_limit_event) 또는 상태 표시줄 연결(rate_limits).
//!           5시간 창은 첫 요청부터 시작하므로 창이 닫혀 있으면 자동 확인 요청을 보내지 않습니다.
//! - Codex:  공식 CLI `app-server`의 account/rateLimits/read (토큰 소모 없음) + Codex 로그.
//! - 로컬 Claude 기록: 쓰는 중인지(토끼 뛰기)와 한도 거절 메시지(낮잠)만.

use crate::accounts::{self, Account};
use crate::bridge::{self, BridgeFile};
use crate::usage_cmd;
use crate::codex::{self, CodexReader, CodexSnapshot};
use crate::config::{self, Config, State};
use crate::login::{self, LoginStatus};
use crate::logs::LogReader;

use crate::texts;
use crate::time::{self, DAY, HOUR};
use crate::tracker::{apply_exact, forecast_from, mood_of, Forecast, ForecastKind, Reading, Status, Tracker};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    pub plan: Option<String>,
    /// Codex 크레딧·리필권
    pub credits: Option<codex::Credits>,
    pub refills: Option<codex::Refills>,
    /// Claude 추가 사용량·지출 한도 (보일 글자)
    pub extra: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub now: f64,
    /// 토끼(귀·기분)를 움직이는 Claude 상태 (위젯에 보이는 첫 Claude 계정)
    pub claude: Option<Status>,
    pub providers: Vec<ProviderView>,
    /// 계정별 로그인 상태 (공식 CLI로 확인)
    pub logins: HashMap<String, LoginStatus>,
    /// 계정별 사용량 확인 상태
    pub probes: HashMap<String, ProbeState>,
}

impl Snapshot {
    pub fn provider(&self, id: ProviderId) -> Option<&ProviderView> {
        self.providers.iter().find(|p| p.id == id)
    }
    pub fn visible(&self) -> impl Iterator<Item = &ProviderView> {
        self.providers.iter().filter(|p| p.show)
    }
}

/// 사용량 확인(Claude 확인 요청 / Codex app-server) 상태
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProbeState {
    pub running: bool,
    /// 마지막으로 시도한 시각 (성공/실패 무관)
    pub tried_at: Option<f64>,
    pub error: Option<String>,
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
    let Some(tried) = tried else { return true }; // 처음 한 번
    let gap = interval_min * 60.0;
    let live = live_at.is_some_and(|at| at > tried + 5.0 && now - at < gap);
    now - tried >= if live { gap * LIVE_STRETCH } else { gap } - 5.0
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

pub fn claude_view(acc: &Account, st: &Status, updated_at: Option<f64>) -> ProviderView {
    let exact = st.exact;
    ProviderView {
        id: ProviderId::Claude,
        account_id: acc.id.clone(),
        name: display_name(ProviderId::Claude, acc),
        meters: vec![
            Meter { label: "5시간".into(), ratio: st.session_pct, resets_at: st.session_reset_at, exact },
            Meter { label: "주간".into(), ratio: st.weekly_pct, resets_at: st.weekly_reset_at, exact },
        ],
        updated_at,
        note: updated_at.map(|t| updated_note(st.now - t)),
        forecast: if exact { Some(texts::forecast_line(st)).filter(|f| !f.is_empty()) } else { None },
        show: acc.show,
        email: None,
        plan: None,
        credits: None,
        refills: None,
        extra: None,
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
                exact: true,
            })
            .collect(),
        updated_at: Some(s.ts),
        note: Some(updated_note(now - s.ts)),
        forecast: None,
        show: acc.show,
        email: s.email.clone(),
        plan: s.plan.clone(),
        credits: s.credits.clone(),
        refills: s.refills.clone(),
        extra: None,
    }
}

// ------------------------------------------------------------------ 알림

/// 사용량별 알림 설정 키
pub fn alert_key(account_id: &str, label: &str) -> String {
    format!("{account_id}:{label}")
}

/// 리셋 임박 알림 시점 범위 (리셋 1분 ~ 7일 전)
pub const MAX_RESET_LEAD_MIN: f64 = 7.0 * 24.0 * 60.0;

/// 사용량 기준 범위 (%)
pub fn pct_in_range(v: f64) -> bool {
    (1.0..100.0).contains(&v)
}

/// 리셋 임박 시점 범위 (리셋 몇 분 전)
pub fn lead_in_range(v: f64) -> bool {
    (1.0..=MAX_RESET_LEAD_MIN).contains(&v)
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
fn alert_list(rule: Option<&serde_json::Value>, field: &str, ok: fn(f64) -> bool) -> Vec<f64> {
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
    let at = alert_list(rule, "at", pct_in_range);
    let before = alert_list(rule, "before", lead_in_range);
    let mut at_off = alert_list(rule, "at_off", pct_in_range);
    at_off.retain(|v| !at.contains(v));
    let mut before_off = alert_list(rule, "before_off", lead_in_range);
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
        // 기본은 아무 알림도 없음: 한도 도달·리셋, 로그인 만료는 사용자가 켜야 알림
        let nap_on = self.state.pref_bool("napNotify", false);
        let login_on = self.state.pref_bool("loginNotify", false);
        let prev_nap = prev.and_then(|p| p.claude.as_ref()).is_some_and(|s| s.nap_kind.is_some());
        if let Some(st) = snap.claude.as_ref().filter(|_| nap_on) {
            if let (Some(kind), Some(wake)) = (st.nap_kind, st.wake_at) {
                let key = format!("{kind:?}:{}", wake as i64);
                if self.state.get_str("nap") != Some(key.as_str()) {
                    self.state.set_str("nap", &key);
                    dirty = true;
                    out.push(texts::nap_notice(st));
                }
            } else if prev_nap {
                out.push(texts::wake_notice());
            }
        }
        // 로그인 만료: 계정마다 한 번 알리고, 다시 로그인하면 초기화
        for (id, l) in snap.logins.iter().filter(|_| login_on) {
            let key = format!("login:{id}");
            match l.logged_in {
                Some(false) if self.state.get_str(&key) != Some("expired") => {
                    self.state.set_str(&key, "expired");
                    dirty = true;
                    let name = snap
                        .providers
                        .iter()
                        .find(|p| &p.account_id == id)
                        .map(|p| p.name.clone())
                        .unwrap_or_else(|| if id.starts_with("codex") { "Codex".into() } else { "Claude".into() });
                    out.push((
                        crate::tr!("로그인 필요", "Sign-in required").into(),
                        crate::tr!(
                            format!("{name} 계정 로그인 만료. usAgI 설정에서 다시 로그인 필요."),
                            format!("{name} sign-in expired. Sign in again in usAgI settings.")
                        ),
                    ));
                }
                Some(true) if self.state.get_str(&key).is_some() => {
                    self.state.0.remove(&key);
                    dirty = true;
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
        // 사용량별 알림: 넘은 기준 중 가장 높은 것 하나만, 창마다 한 번
        for p in snap.providers.iter().filter(|p| p.show) {
            for m in p.meters.iter().filter(|m| m.exact) {
                let key = alert_key(&p.account_id, &m.label);
                let (on, thresholds) = alert_rule(&self.state, &key);
                if !on || m.ratio >= 1.0 {
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
                    let st = if p.id == ProviderId::Claude { snap.claude.as_ref() } else { None };
                    out.push(texts::tired_notice(&p.name, &m.label, m.ratio, st));
                }
            }
        }
        // 리셋 임박: 켜 둔 시점 중 지금 맞는 가장 작은 것 하나만, 창마다 한 번
        for p in snap.providers.iter().filter(|p| p.show) {
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
        if dirty {
            self.state.save();
        }
        out
    }
}

// ------------------------------------------------------------------ 소스

struct ClaudeSource {
    acc: Account,
    reader: LogReader,
    tracker: Tracker,
    need_update: bool,
    /// 정확한 값 기록 (낮잠 예보용)
    five: Vec<Reading>,
    seven: Vec<Reading>,
    last_seen: f64,
}

struct CodexSource {
    acc: Account,
    reader: CodexReader,
}

struct Sources {
    claude: Vec<ClaudeSource>,
    codex: Vec<CodexSource>,
    /// 설정 화면에서 정한 계정 순서 (위젯·메뉴 막대도 이 순서)
    order: Vec<String>,
}

impl Sources {
    fn build(cfg: &Config, accounts: &[Account]) -> Self {
        let mut claude = Vec::new();
        let mut codex = Vec::new();
        for acc in accounts {
            match acc.provider.as_str() {
                "claude" if cfg.providers.iter().any(|p| p == "claude") => {
                    let mut roots = vec![acc.path().join("projects")];
                    roots.extend(cfg.claude_dirs.iter().map(PathBuf::from));
                    claude.push(ClaudeSource {
                        acc: acc.clone(),
                        reader: LogReader::new(roots),
                        tracker: Tracker::new(),
                        need_update: true,
                        five: Vec::new(),
                        seven: Vec::new(),
                        last_seen: 0.0,
                    });
                }
                "codex" if cfg.providers.iter().any(|p| p == "codex") => codex.push(CodexSource {
                    acc: acc.clone(),
                    reader: CodexReader::new(vec![acc.path().join("sessions"), acc.path().join("archived_sessions")]),
                }),
                _ => {}
            }
        }
        Sources { claude, codex, order: accounts.iter().map(|a| a.id.clone()).collect() }
    }

    fn snapshot(&mut self, cfg: &Config) -> Snapshot {
        let now = time::now();
        let tired = cfg.tired_percent / 100.0;
        let mut providers = Vec::new();
        let mut rabbit: Option<(bool, Status)> = None;
        for src in &mut self.claude {
            if src.reader.poll() || src.need_update {
                let limits = src.reader.limit_events();
                src.tracker.update(src.reader.usage_events(), limits);
                src.need_update = false;
            }
            let mut st = src.tracker.status(now);
            let file = bridge::read(&src.acc.id);
            if let Some(b) = &file {
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
            let mut view = claude_view(&src.acc, &st, file.as_ref().map(|b| b.updated_at));
            view.extra = file.as_ref().and_then(|b| b.spend.as_ref().map(|s| s.label()).or_else(|| b.extra.clone()));
            if let Some(b) = file.as_ref().filter(|_| st.exact) {
                for m in &b.models {
                    view.meters.push(Meter {
                        label: format!("{} 주간", m.name),
                        ratio: m.window.ratio_at(now),
                        resets_at: m.window.live_reset(now).or_else(|| b.weekly_reset(now)),
                        exact: true,
                    });
                }
            }
            providers.push(view);
            // 토끼는 위젯에 보이는 첫 Claude 계정을 따라감
            let better = match &rabbit {
                None => true,
                Some((shown, _)) => !shown && src.acc.show,
            };
            if better {
                rabbit = Some((src.acc.show, st));
            }
        }
        for src in &mut self.codex {
            src.reader.poll();
            // 로그 값과 app-server 값 중 최신
            let live = codex::load(&src.acc.id);
            let best = match (&src.reader.snapshot, &live) {
                (Some(a), Some(b)) if a.ts >= b.ts => {
                    let mut s = a.clone();
                    s.email = s.email.or(b.email.clone());
                    s.plan = s.plan.or(b.plan.clone());
                    s.credits = s.credits.or(b.credits.clone());
                    s.refills = s.refills.or(b.refills.clone());
                    Some(s)
                }
                (Some(_), Some(b)) => Some(b.clone()),
                (a, b) => a.clone().or(b.clone()),
            };
            if let Some(s) = best {
                providers.push(codex_view(&src.acc, &s, now));
            }
        }
        sort_by_order(&mut providers, &self.order);
        Snapshot { now, claude: rabbit.map(|(_, st)| st), providers, logins: HashMap::new(), probes: HashMap::new() }
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
    Stop,
}

pub struct Engine {
    tx: Sender<Cmd>,
    cfg: Arc<Mutex<Config>>,
    login_tx: Sender<()>,
    /// Some(계정) = 그 계정만, None = 모든 계정을 지금 확인
    probe_tx: Sender<Option<String>>,
}

/// 로컬 기록은 서버 요청이 아니므로 짧게(3초) 다시 읽음
const LOCAL_POLL: Duration = Duration::from_secs(3);
/// 바뀐 게 없어도 이 간격으로는 화면에 알림 (남은 시간 글자 갱신용)
const QUIET_SECS: f64 = 30.0;

/// 화면에 보이는 내용이 달라졌는지 비교할 요약. 매번 달라지는 시각(now, 쉰 시간)은 빼요.
#[derive(PartialEq)]
struct Shown {
    providers: Vec<ProviderView>,
    logins: HashMap<String, LoginStatus>,
    probes: HashMap<String, ProbeState>,
    rabbit: Option<String>,
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
        Shown { providers: snap.providers.clone(), logins: snap.logins.clone(), probes: snap.probes.clone(), rabbit }
    }
}

impl Engine {
    /// 백그라운드 스레드 시작. on_snapshot은 다른 스레드에서 불리므로 UI 스레드로 넘기세요.
    pub fn start<F: Fn(Snapshot) + Send + 'static>(on_snapshot: F) -> Engine {
        // 첫 스냅숏·설정 파일 오류·새 설정 파일도 저장된 언어로 (컨트롤러보다 먼저 시작함)
        State::load().apply_locale();
        let cfg = Arc::new(Mutex::new(config::load()));
        let (tx, rx) = mpsc::channel();

        // 로그인 상태: 한 시간마다(또는 요청 시) 공식 CLI로 확인
        let logins: Arc<Mutex<HashMap<String, LoginStatus>>> = Arc::new(Mutex::new(HashMap::new()));
        let (login_tx, login_rx) = mpsc::channel::<()>();
        {
            let logins = logins.clone();
            let poke = tx.clone();
            std::thread::Builder::new()
                .name("usagi-login".into())
                .spawn(move || loop {
                    for acc in accounts::load(&State::load()) {
                        let st = login::status(&acc);
                        if let Ok(mut g) = logins.lock() {
                            g.insert(acc.id.clone(), st);
                        }
                    }
                    let _ = poke.send(Cmd::Poke);
                    match login_rx.recv_timeout(LOGIN_CHECK) {
                        Ok(()) | Err(RecvTimeoutError::Timeout) => while login_rx.try_recv().is_ok() {},
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                })
                .expect("spawn login thread");
        }

        // 사용량 확인: 갱신 주기마다 (쓰는 중이라 새 값이 따로 들어오면 덜 자주), 새로고침이면 즉시
        let probes: Arc<Mutex<HashMap<String, ProbeState>>> = Arc::new(Mutex::new(HashMap::new()));
        // Codex 실행 기록에서 마지막으로 새 값을 읽은 시각 (계정별, 기록을 읽는 쪽이 채움)
        let live: Arc<Mutex<HashMap<String, f64>>> = Arc::new(Mutex::new(HashMap::new()));
        let (probe_tx, probe_rx) = mpsc::channel::<Option<String>>();
        {
            let probes = probes.clone();
            let logins = logins.clone();
            let live = live.clone();
            let poke = tx.clone();
            std::thread::Builder::new()
                .name("usagi-probe".into())
                .spawn(move || {
                    let mut wait = Duration::from_secs(5); // 처음엔 로그인 상태 확인만 기다렸다가 바로
                    loop {
                        let forced: Option<Option<String>> = match probe_rx.recv_timeout(wait) {
                            Ok(x) => Some(x),
                            Err(RecvTimeoutError::Timeout) => None,
                            Err(RecvTimeoutError::Disconnected) => break,
                        };
                        wait = Duration::from_secs(30);
                        let state = State::load();
                        let interval = refresh_minutes(&state);
                        for acc in accounts::load(&state) {
                            let force = matches!(&forced, Some(None)) || matches!(&forced, Some(Some(id)) if *id == acc.id);
                            let logged_out =
                                logins.lock().ok().and_then(|g| g.get(&acc.id).and_then(|l| l.logged_in)) == Some(false);
                            if !force && logged_out {
                                continue;
                            }
                            let now = time::now();
                            let tried = probes.lock().ok().and_then(|g| g.get(&acc.id).and_then(|p| p.tried_at));
                            let due = match acc.provider.as_str() {
                                // /usage는 토큰을 쓰지 않고 창도 열지 않음. 상태 표시줄로 값이 들어오는 중이면 덜 자주
                                "claude" if usage_cmd::supported(&acc.id) => {
                                    usage_due(tried, bridge::read(&acc.id).map(|b| b.updated_at), interval, now)
                                }
                                // Codex 읽기도 토큰 0. Codex를 쓰는 중이면 실행 기록에 값이 남으므로 덜 자주
                                "codex" => usage_due(tried, live.lock().ok().and_then(|g| g.get(&acc.id).copied()), interval, now),
                                _ => false,
                            };
                            if !force && !due {
                                continue;
                            }
                            if let Ok(mut g) = probes.lock() {
                                g.entry(acc.id.clone()).or_default().running = true;
                            }
                            let _ = poke.send(Cmd::Poke);
                            let result = match acc.provider.as_str() {
                                "claude" if !usage_cmd::supported(&acc.id) => Err(crate::tr!(
                                    "설치된 Claude Code 버전은 사용량 조회 미지원. Claude Code 업데이트 필요",
                                    "This Claude Code version does not support usage checks. Update Claude Code."
                                )
                                .into()),
                                "claude" => match usage_cmd::run(&acc) {
                                    Ok(file) => {
                                        bridge::save(&acc.id, file);
                                        Ok(())
                                    }
                                    Err(e) => Err(e),
                                },
                                _ => codex::read_via_app_server(&acc).map(|s| codex::save(&acc.id, &s)),
                            };
                            if let Ok(mut g) = probes.lock() {
                                let p = g.entry(acc.id.clone()).or_default();
                                p.running = false;
                                p.tried_at = Some(time::now());
                                p.error = result.err();
                            }
                            let _ = poke.send(Cmd::Poke);
                        }
                    }
                })
                .expect("spawn probe thread");
        }

        let shared = cfg.clone();
        std::thread::Builder::new()
            .name("usagi-poll".into())
            .spawn(move || {
                let mut c = shared.lock().map(|g| g.clone()).unwrap_or_default();
                let state = State::load();
                config::apply_prefs(&mut c, &state);
                let mut sources = Sources::build(&c, &accounts::load(&state));
                // 바뀐 게 없으면 화면 쪽(위젯·설정 창)을 깨우지 않음. 요청(새로고침·확인 끝)이 오면 바로 보냄.
                let mut last: Option<Shown> = None;
                let mut last_sent = 0.0;
                let mut force = true;
                loop {
                    let mut snap = sources.snapshot(&c);
                    if let Ok(mut g) = live.lock() {
                        for src in &sources.codex {
                            if let Some(s) = &src.reader.snapshot {
                                g.insert(src.acc.id.clone(), s.ts);
                            }
                        }
                    }
                    if let Ok(g) = logins.lock() {
                        for (id, login) in g.iter() { snap.logins.entry(id.clone()).or_insert_with(|| login.clone()); }
                    }
                    if let Ok(g) = probes.lock() {
                        for (id, probe) in g.iter() { snap.probes.entry(id.clone()).or_insert_with(|| probe.clone()); }
                    }
                    let shown = Shown::of(&snap);
                    if force || last.as_ref() != Some(&shown) || snap.now - last_sent >= QUIET_SECS {
                        last = Some(shown);
                        last_sent = snap.now;
                        on_snapshot(snap);
                    }
                    force = false;
                    match rx.recv_timeout(LOCAL_POLL) {
                        Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                        Ok(Cmd::Poke) => force = true,
                        Ok(Cmd::Refresh) => {
                            force = true;
                            c = config::load();
                            let state = State::load();
                            config::apply_prefs(&mut c, &state);
                            if let Ok(mut g) = shared.lock() {
                                *g = c.clone();
                            }
                            sources = Sources::build(&c, &accounts::load(&state));
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
        let _ = self.probe_tx.send(account_id);
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
        let _ = self.tx.send(Cmd::Stop);
    }

    pub fn config(&self) -> Config {
        self.cfg.lock().map(|g| g.clone()).unwrap_or_default()
    }

    /// 테스트용: 스레드 없이 명령을 버리는 엔진 (CLI를 실행하지 않음)
    #[cfg(test)]
    pub(crate) fn idle() -> Engine {
        let (tx, _) = mpsc::channel();
        let (login_tx, _) = mpsc::channel();
        let (probe_tx, _) = mpsc::channel();
        Engine { tx, cfg: Arc::new(Mutex::new(Config::default())), login_tx, probe_tx }
    }
}

/// 한 번만 계산 (CLI `--status` 등)
pub fn snapshot_once() -> Snapshot {
    let state = State::load();
    state.apply_locale();
    let c = config::load();
    Sources::build(&c, &accounts::load(&state)).snapshot(&c)
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
                plan: None,
                credits: None,
                refills: None,
                extra: None,
            }],
            logins: HashMap::new(),
            probes: HashMap::new(),
        };
        let cfg = Config::default();
        assert_eq!(n.check(None, &mk(0.4), &cfg).len(), 0);
        assert_eq!(n.check(None, &mk(0.55), &cfg).len(), 1); // 50%
        assert_eq!(n.check(None, &mk(0.6), &cfg).len(), 0); // 같은 기준 다시 안 알림
        assert_eq!(n.check(None, &mk(0.85), &cfg).len(), 1); // 80%
        assert_eq!(n.check(None, &mk(0.9), &cfg).len(), 0);
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
                plan: None,
                credits: None,
                refills: None,
                extra: None,
            }],
            logins: HashMap::new(),
            probes: HashMap::new(),
        }
    }

    #[test]
    fn reset_soon_fires_once_per_window_with_the_smallest_lead() {
        let mut n = Notices { state: State::default() };
        n.state.0.insert("alerts".into(), serde_json::json!({ "claude-1:5시간": { "before": [10, 30, 60] } }));
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
    fn login_expiry_notice_only_when_turned_on() {
        let mut n = Notices { state: State::default() };
        let mut snap = Snapshot { now: time::now(), claude: None, providers: vec![], logins: HashMap::new(), probes: HashMap::new() };
        snap.logins.insert("claude-1".into(), LoginStatus { logged_in: Some(false), ..Default::default() });
        let cfg = Config::default();
        assert!(n.check(None, &snap, &cfg).is_empty(), "기본은 로그인 만료 알림 없음");
        n.state.set_pref("loginNotify", serde_json::json!(true));
        assert_eq!(n.check(None, &snap, &cfg).len(), 1);
        assert!(n.check(None, &snap, &cfg).is_empty(), "한 번만");
    }

    #[test]
    fn claude_view_hides_values_without_exact() {
        let t = Tracker::new();
        let st = t.status(time::now());
        let acc = Account { id: "claude-1".into(), provider: "claude".into(), dir: ".".into(), label: "회사".into(), show: true, chain: None };
        let v = claude_view(&acc, &st, None);
        assert_eq!(v.name, "Claude (회사)");
        assert!(v.meters.iter().all(|m| !m.exact));
        assert!(v.forecast.is_none());
        assert_eq!(st.mood, Mood::Rest);
    }
}
