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
use crate::probe;
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
        "방금".into()
    } else {
        format!("{} 전", texts::fmt_duration(secs))
    }
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
        note: updated_at.map(|t| format!("{} 기준", age_text(st.now - t))),
        forecast: if exact { Some(texts::forecast_line(st)).filter(|f| !f.is_empty()) } else { None },
        show: acc.show,
        email: None,
        plan: None,
        credits: None,
        refills: None,
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
        note: Some(format!("{} 기준", age_text(now - s.ts))),
        forecast: None,
        show: acc.show,
        email: s.email.clone(),
        plan: s.plan.clone(),
        credits: s.credits.clone(),
        refills: s.refills.clone(),
    }
}

// ------------------------------------------------------------------ 알림

/// 사용량별 알림 설정 키
pub fn alert_key(account_id: &str, label: &str) -> String {
    format!("{account_id}:{label}")
}

/// 알림 규칙: alerts["계정:라벨"] = {"on": bool, "at": [50, 80, 95]} (예전 형식 "at": 80도 읽음).
/// 기본은 기준 없음: 사용자가 그래프에 추가한 기준에서만 알려요.
pub fn alert_rule(state: &State, key: &str) -> (bool, Vec<f64>) {
    let rule = state.0.get("alerts").and_then(|a| a.get(key));
    let on = rule.and_then(|r| r.get("on")).and_then(|v| v.as_bool()).unwrap_or(true);
    let mut at: Vec<f64> = match rule.and_then(|r| r.get("at")) {
        Some(serde_json::Value::Array(a)) => a.iter().filter_map(|v| v.as_f64()).collect(),
        Some(v) => v.as_f64().into_iter().collect(),
        None => Vec::new(),
    };
    at.retain(|v| (1.0..100.0).contains(v));
    at.sort_by(|a, b| a.total_cmp(b));
    at.dedup();
    (on, at)
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
                    out.push(("로그인 필요".into(), format!("{name} 계정 로그인 만료. usAgI 설정에서 다시 로그인 필요.")));
                }
                Some(true) if self.state.get_str(&key).is_some() => {
                    self.state.0.remove(&key);
                    dirty = true;
                }
                _ => {}
            }
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
        let cfg = Arc::new(Mutex::new(config::load()));
        let (tx, rx) = mpsc::channel();

        // 로그인 상태: 15분마다(또는 요청 시) 공식 CLI로 확인
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
                    match login_rx.recv_timeout(Duration::from_secs(15 * 60)) {
                        Ok(()) | Err(RecvTimeoutError::Timeout) => while login_rx.try_recv().is_ok() {},
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                })
                .expect("spawn login thread");
        }

        // 사용량 확인: 갱신 주기마다 (Claude는 창이 열려 있을 때만), 새로고침이면 즉시
        let probes: Arc<Mutex<HashMap<String, ProbeState>>> = Arc::new(Mutex::new(HashMap::new()));
        let (probe_tx, probe_rx) = mpsc::channel::<Option<String>>();
        {
            let probes = probes.clone();
            let logins = logins.clone();
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
                            let logged_out =
                                logins.lock().ok().and_then(|g| g.get(&acc.id).and_then(|l| l.logged_in)) == Some(false);
                            if logged_out {
                                continue;
                            }
                            let now = time::now();
                            let tried = probes.lock().ok().and_then(|g| g.get(&acc.id).and_then(|p| p.tried_at));
                            let force = matches!(&forced, Some(None)) || matches!(&forced, Some(Some(id)) if *id == acc.id);
                            let due = match acc.provider.as_str() {
                                // /usage는 토큰을 쓰지 않고 창도 열지 않으므로 주기마다. 안 되는 CLI면 기존 규칙
                                "claude" if usage_cmd::supported(&acc.id) => tried.is_none_or(|t| now - t >= interval * 60.0 - 5.0),
                                "claude" => probe_due(bridge::read(&acc.id).as_ref(), tried, interval, now),
                                // Codex 읽기는 토큰을 쓰지 않으므로 창과 상관없이 주기마다
                                "codex" => tried.is_none_or(|t| now - t >= interval * 60.0 - 5.0),
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
                                "claude" => match usage_cmd::run(&acc) {
                                    Ok(file) => {
                                        bridge::save(&acc.id, file);
                                        Ok(())
                                    }
                                    // /usage가 없는 버전: 창이 열려 있을 때만 아주 작은 요청으로
                                    Err(_) if !usage_cmd::supported(&acc.id)
                                        && (force || probe_due(bridge::read(&acc.id).as_ref(), None, interval, now)) =>
                                    {
                                        probe::run(&acc).map(|o| bridge::save(&acc.id, o.file))
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
                    if let Ok(g) = logins.lock() {
                        snap.logins = g.clone();
                    }
                    if let Ok(g) = probes.lock() {
                        snap.probes = g.clone();
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
    let c = config::load();
    Sources::build(&c, &accounts::load(&State::load())).snapshot(&c)
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
            models: Vec::new(),
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
