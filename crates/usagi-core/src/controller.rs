//! 설정 화면 컨트롤러 — 화면 상태(JSON)와 화면에서 오는 모든 명령을 플랫폼과 상관없이 한 곳에서 처리.
//!
//! Windows·macOS 앱은 같은 Slint 화면을 띄우고:
//! 1. 화면에서 온 명령을 [`Controller::handle_ipc`]에 넘기고
//! 2. 돌려받은 [`Effect`]만 실행해요 (주소 열기, 알림, 시작 시 실행, 위젯·메뉴 막대 다시 그리기, 업데이트 받기·적용).
//! 3. 화면에 보낼 상태는 [`Controller::settings_state`]가 만들어요. 플랫폼은 몇 가지 값([`HostInfo`])만 채워요.
//!
//! 계정 추가·삭제, 로그인 진행, 알림 기준, 그래프별 설정, 업데이트 흐름이 모두 여기 있어서
//! 플랫폼마다 동작이 달라지지 않아요.

use crate::accounts::{self, Account, Link};
use crate::config::{self, Config, State};
use crate::connection::{Connection, Destination};
use crate::engine::{self, Engine, Notices, Snapshot};
use crate::login::{self, LoginSession};
use crate::update::{self, Release};
use crate::{texts, time, ui};
use serde_json::{json, Value};
use std::path::PathBuf;

/// 공식 사용량 페이지 (정해진 주소만 엶)
pub const USAGE_URL_CLAUDE: &str = "https://claude.ai/settings/usage";
pub const USAGE_URL_CODEX: &str = "https://chatgpt.com/codex/settings/usage";


const LOGIN_POLL_SECS: f64 = 0.4;
const CONNECTION_POLL_SECS: f64 = 0.2;
const PROFILE_DELETE_POLL_SECS: f64 = 1.0;
/// 로그인이 끝난 뒤 CLI가 상태를 쓰는 데 잠깐 걸릴 수 있어 몇 번 더 확인
const LOGIN_RECHECKS: u32 = 4;
const LOGIN_RECHECK_SECS: f64 = 3.0;
/// 성공한 로그인 결과를 보여 주는 시간 (실패는 사용자가 닫을 때까지)
const LOGIN_DONE_SHOW_SECS: f64 = 6.0;
/// 자동 업데이트 확인: 시작 1분 뒤, 그다음 6시간마다
const UPDATE_FIRST_SECS: f64 = 60.0;
const UPDATE_EVERY_SECS: f64 = 6.0 * 60.0 * 60.0;

/// 플랫폼 앱이 실행할 일
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// 설정 화면에 상태를 다시 보냄
    Push,
    /// 위젯·메뉴 막대 다시 그리기 (표시 설정·계정이 바뀜)
    Redraw,
    /// 저장 전 막대 길이로 Windows 위젯과 설정 미리 보기 갱신
    WidgetBarLength,
    /// 플랫폼 전용 표시 설정이 바뀜 (값은 이미 저장됨): "widget", "side", "monitor", "urgent"
    PlatformPref(String),
    /// https 주소나 폴더를 기본 앱으로 열기
    Open(String),
    /// 텍스트 파일을 편집기로 열기
    OpenText(PathBuf),
    /// 시스템 알림 설정 열기
    OpenNotifySettings,
    /// 알림 (제목, 내용)
    Notify(String, String),
    /// 시작(로그인) 시 실행 켜기/끄기. 실패하면 플랫폼이 [`Controller::set_error`]
    SetAutostart(bool),
    /// 설정 창 닫기 → 닫은 뒤 [`Controller::settings_closed`]
    CloseSettings,
    /// 앱 종료
    Quit,
    /// 업데이트 확인 (백그라운드) → 끝나면 [`Controller::update_checked`]
    CheckUpdate { manual: bool },
    /// 새 버전 받기 (백그라운드) → 끝나면 [`Controller::update_downloaded`]
    DownloadUpdate { release: Release, install: bool },
    /// 받아 둔 새 버전으로 바꾸고 다시 시작. 성공하면 앱 종료, 실패하면 [`Controller::update_apply_failed`]
    ApplyUpdate(PathBuf),
}

/// 플랫폼 고정 정보 (시작할 때 한 번)
#[derive(Clone, Debug)]
pub struct Platform {
    /// "windows" | "mac" | "linux" — 화면이 플랫폼 전용 항목을 보이거나 숨김
    pub name: &'static str,
    /// 업데이트 저장소 ("owner/name")
    pub repo: &'static str,
    /// 패키지 관리자(Homebrew 등)가 버전을 관리해 스스로 바꾸지 않음 → 이 업데이트 명령을 안내
    pub managed_update: Option<&'static str>,
}

/// 설정 화면 상태를 만들 때 플랫폼이 채우는 값 (매번 시스템에서 읽음)
#[derive(Clone, Debug, Default)]
pub struct HostInfo {
    /// 시작(로그인) 시 실행이 켜져 있는지
    pub autostart: bool,
    /// 방해 금지 중에도 보이는 알림을 지원 (Windows 중요 알림)
    pub urgent_supported: bool,
    /// 시스템이 그 알림을 허용했는지 (모르면 None)
    pub urgent_allowed: Option<bool>,
    /// 위젯을 놓을 모니터 목록 [{id, label}] (없으면 비움)
    pub monitors: Vec<Value>,
    /// 위젯 막대 길이 범위 (논리 px): 최소(가장 긴 남은 시간 글자), 기본(예전 고정 길이), 최대. 위젯이 없으면 None
    pub bar_range: Option<(f64, f64, f64)>,
    /// 작업 표시줄이 밝은지 (Windows 모드). 설정 창의 작업 표시줄 아이콘 색을 고름. 모르면 None
    pub taskbar_light: Option<bool>,
    /// 위젯 미리 보기 (Windows가 지금 설정대로 그린 그림: {w, h, scale, light, rgba}). 바뀌지 않았으면 {keep: true}, 보일 게 없으면 None
    pub widget_preview: Option<Value>,
}

#[derive(Default)]
struct UpdateState {
    busy: Option<&'static str>,
    latest: Option<Release>,
    staged: Option<PathBuf>,
    manual_install: bool,
    checked_at: Option<f64>,
    error: Option<String>,
}

pub struct Controller {
    pub engine: Engine,
    pub notices: Notices,
    pub snap: Option<Snapshot>,
    pub platform: Platform,
    last_error: Option<String>,
    install_help: Option<String>,
    /// 콘솔 없이 돌고 있는 공식 로그인 (한 번에 하나)
    login: Option<LoginSession>,
    connecting: Option<Connection>,
    login_finished_at: Option<f64>,
    last_login_poll: f64,
    login_rechecks: u32,
    next_recheck: f64,
    /// "추가"로 만들어 공식 로그인 중인, 아직 등록 전인 프로필
    pending_new: Option<Account>,
    inactive_profiles: Vec<PathBuf>,
    deleted_profiles: Vec<(PathBuf, std::sync::mpsc::Receiver<std::io::Result<()>>)>,
    update: UpdateState,
    next_update_check: f64,
    next_rule_check: Option<f64>,
    settings_open: bool,
    widget_preview_open: bool,
    widget_bar_draft: Option<f64>,
}

impl Controller {
    pub fn new(engine: Engine, platform: Platform) -> Self {
        let mut notices = Notices::load();
        crate::i18n::set(crate::i18n::resolve(&notices.state.pref_str("locale", "system")));
        if notices.state.0.remove("tiers").is_some() {
            notices.state.save(); // 예전 수동 요금제 단계 (이제 자동만)
        }
        Controller {
            engine,
            notices,
            snap: None,
            platform,
            last_error: None,
            install_help: None,
            login: None,
            connecting: None,
            login_finished_at: None,
            last_login_poll: 0.0,
            login_rechecks: 0,
            next_recheck: 0.0,
            pending_new: None,
            inactive_profiles: Vec::new(),
            deleted_profiles: Vec::new(),
            update: UpdateState::default(),
            next_update_check: time::now() + UPDATE_FIRST_SECS,
            next_rule_check: None,
            settings_open: false,
            widget_preview_open: false,
            widget_bar_draft: None,
        }
    }

    pub fn state(&self) -> &State {
        &self.notices.state
    }

    /// 설정 화면에 보이는 오류 (진단 기록용)
    pub fn error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn set_error(&mut self, e: impl Into<String>) {
        self.last_error = Some(e.into());
    }

    fn save_settings(&mut self, previous: State) -> bool {
        match self.notices.state.try_save() {
            Ok(()) => true,
            Err(error) => {
                self.notices.state = previous;
                self.set_error(storage_error(&error));
                false
            }
        }
    }

    pub fn effective_config(&self) -> Config {
        let mut c = self.engine.config();
        config::apply_prefs(&mut c, &self.notices.state);
        c
    }

    /// 위젯·메뉴 막대 값: 남은 시간으로 보기
    pub fn time_mode(&self) -> bool {
        self.notices.state.pref_str("mode", "percent") == "time"
    }

    pub fn auto_update(&self) -> bool {
        self.platform.managed_update.is_none() && self.notices.state.pref_bool("autoUpdate", true)
    }

    // -------------------------------------------------------------- 생명 주기

    /// 앱이 뜬 직후: 설정 파일 오류, 방금 업데이트했으면 알림
    pub fn startup(&mut self) -> Vec<Effect> {
        let mut fx = Vec::new();
        self.ensure_links(None);
        if accounts::has_unread_accounts(&self.notices.state) {
            self.set_error(crate::tr!(
                "일부 계정 정보 읽기 실패 · 기존 데이터 보관",
                "Some account entries could not be read · Existing data preserved"
            ));
        }
        if let Err(error) = accounts::protect_profiles() {
            let message = storage_error(&error);
            self.set_error(message.clone());
            fx.push(Effect::Notify(crate::tr!("프로필 권한 설정 실패", "Could not protect profile folders").into(), message));
        }
        if let Some(err) = self.engine.config().error {
            fx.push(Effect::Notify(crate::tr!("usAgI 설정 오류", "usAgI config error").into(), err));
        }
        let last = self.notices.state.get_str("last_version").map(str::to_string);
        if last.as_deref() != Some(crate::VERSION) {
            if last.is_some() {
                fx.push(Effect::Notify(
                    crate::tr!("usAgI 업데이트 완료", "usAgI updated").into(),
                    crate::tr!(format!("v{}로 업데이트 완료.", crate::VERSION), format!("Updated to v{}.", crate::VERSION)),
                ));
            }
            self.notices.state.set_str("last_version", crate::VERSION);
            self.notices.state.save();
        }
        fx
    }

    /// 엔진이 새 사용량을 보냄 → 알림 판단
    pub fn apply_snapshot(&mut self, mut snap: Snapshot) -> Vec<Effect> {
        let mut accounts = accounts::load(&self.notices.state);
        snap.retain_current(&accounts);
        let mut changed = false;
        for account in accounts.iter_mut().filter(|account| account.identity.is_none()) {
            let from_status = || snap.logins.get(&account.id).filter(|status| status.logged_in == Some(true))
                .and_then(login::Identity::from_status);
            account.identity = if account.is_claude() {
                from_status()
            } else {
                crate::codex::load(&account.usage_key()).as_ref().and_then(login::Identity::from_codex).or_else(from_status)
            };
            changed |= account.identity.is_some();
        }
        if changed {
            self.store_accounts(&accounts);
        }
        let cfg = self.effective_config();
        let mut fx: Vec<Effect> = self.notices.check(self.snap.as_ref(), &snap, &cfg).into_iter().map(|(t, m)| Effect::Notify(t, m)).collect();
        self.next_rule_check = if cfg.notify { crate::alert_rules::next_check(self.state(), &snap, snap.now) } else { None };
        self.snap = Some(snap);
        fx.push(Effect::Redraw);
        fx.push(Effect::Push);
        fx
    }

    /// 자주 불러 주세요 (0.2초 안팎). 로그인 진행 확인, 로그인 후 재확인, 업데이트 확인 시각을 스스로 셈.
    pub fn tick(&mut self) -> Vec<Effect> {
        let now = time::now();
        let mut fx = Vec::new();
        if self.next_rule_check.is_some_and(|at| now >= at) {
            self.next_rule_check = None;
            if let Some(snap) = self.snap.as_ref().filter(|_| self.effective_config().notify) {
                let notices = self.notices.compound_usage(snap, now);
                if !notices.is_empty() { self.notices.state.save(); }
                fx.extend(notices.into_iter().map(|(title, body)| Effect::Notify(title, body)));
                self.next_rule_check = crate::alert_rules::next_check(self.state(), snap, now);
            }
        }
        self.poll_connection(&mut fx);
        if self.poll_profile_deletions() {
            fx.push(Effect::Push);
        }
        if self.login.is_some() && now - self.last_login_poll >= LOGIN_POLL_SECS {
            self.last_login_poll = now;
            if self.poll_login(now) {
                fx.push(Effect::Push);
            }
        }
        if self.login_rechecks > 0 && now >= self.next_recheck {
            self.engine.check_logins();
            self.login_rechecks -= 1;
            self.next_recheck = now + LOGIN_RECHECK_SECS;
            if self.login_rechecks == 0 {
                fx.push(Effect::Push);
            }
        }
        if now >= self.next_update_check {
            self.next_update_check = now + UPDATE_EVERY_SECS;
            if self.notices.state.pref_bool("updateCheck", true) {
                fx.extend(self.check_update(false));
            }
        }
        if let Some(effect) = self.apply_staged_when_ready(false) {
            fx.push(effect);
        }
        fx
    }

    /// 로그인 진행을 보고 있어 tick을 자주(0.2초쯤) 불러야 하는지. 아니면 1초에 한 번이면 충분해요.
    pub fn busy(&self) -> bool {
        self.login.is_some() || self.connecting.is_some()
    }

    /// Earliest deadline for hosts that can sleep until the next controller task.
    pub fn next_tick_delay(&self, now: f64) -> Option<std::time::Duration> {
        [
            self.busy().then_some(now + CONNECTION_POLL_SECS),
            (!self.deleted_profiles.is_empty()).then_some(now + PROFILE_DELETE_POLL_SECS),
            (self.login_rechecks > 0).then_some(self.next_recheck),
            self.state().pref_bool("updateCheck", true).then_some(self.next_update_check),
            self.next_rule_check,
            (self.update.staged.is_some() && self.can_apply_update(false)).then_some(now),
        ]
        .into_iter()
        .flatten()
        .min_by(f64::total_cmp)
        .map(|at| std::time::Duration::from_secs_f64((at - now).max(0.0)))
    }

    pub fn settings_opened(&mut self) {
        self.settings_open = true;
        self.reload_inactive_profiles();
        self.engine.check_logins();
    }

    pub fn wants_widget_preview(&self) -> bool {
        self.widget_preview_open
    }

    pub fn widget_bar_px(&self) -> Option<f64> {
        self.widget_bar_draft.or_else(|| self.state().pref_f64("barPx"))
    }

    fn preview_widget_bar_length(&mut self, value: Option<f64>) -> Vec<Effect> {
        let previous = self.widget_bar_px();
        self.widget_bar_draft = value;
        if previous == self.widget_bar_px() { Vec::new() } else { vec![Effect::WidgetBarLength] }
    }

    /// 설정 창을 닫음 → 받아 둔 업데이트가 있으면 이제 적용
    pub fn settings_closed(&mut self) -> Vec<Effect> {
        self.settings_open = false;
        self.widget_preview_open = false;
        let mut effects = self.preview_widget_bar_length(None);
        effects.extend(self.apply_staged_when_ready(false));
        effects
    }

    /// 모든 계정 다시 확인 (메뉴의 "지금 새로고침")
    pub fn refresh_all(&self) {
        self.engine.probe_now(None);
        self.engine.refresh();
        self.engine.check_logins();
    }

    /// 퍼센트 ↔ 남은 시간 (위젯 우클릭, 메뉴 막대 메뉴)
    pub fn toggle_mode(&mut self) -> Vec<Effect> {
        let next = if self.time_mode() { "percent" } else { "time" };
        self.set_pref("mode", json!(next))
    }

    // -------------------------------------------------------------- 설정 화면 상태

    pub fn settings_state(&self, host: &HostInfo) -> Value {
        let st = &self.notices.state;
        let cfg = self.effective_config();
        let now = time::now();
        json!({
            "platform": self.platform.name,
            // 지금 쓰는 언어 (설정 창이 같은 언어로 그림)
            "lang": crate::i18n::lang().code(),
            "version": crate::VERSION,
            "update": self.update_json(),
            "portable": config::is_portable(),
            "dataDir": config::config_dir().to_string_lossy(),
            "prefs": {
                "widget": st.pref_bool("widget", true),
                "side": st.pref_str("side", "right"),
                "monitor": st.pref_str("monitor", "primary"),
                "mode": st.pref_str("mode", "percent"),
                "theme": st.pref_str("theme", "system"),
                "logo": st.pref_str("logo", "auto"),
                "locale": st.pref_str("locale", "system"),
                // 막대 길이(px, 남은 시간 표시일 때). 설정이 없으면 기본(예전 고정 길이)
                "barPx": st.pref_f64("barPx"),
                "barMin": host.bar_range.map(|r| r.0),
                "barDefault": host.bar_range.map(|r| r.1),
                "barMax": host.bar_range.map(|r| r.2),
                // 위젯에 막대를 그릴지 (끄면 라벨과 값만)
                "bars": st.pref_bool("bars", true),
                "basis": st.pref_str("basis", "used"),
                "refresh": engine::refresh_minutes(st),
                "rabbit": st.pref_bool("rabbit", true),
                "notify": cfg.notify,
                "napNotify": st.pref_bool("napNotify", false),
                "loginNotify": st.pref_bool("loginNotify", true),
                "loginWarningDays": login::SIGN_IN_WARNING_DAYS,
                "refillNotify": st.pref_bool("refillNotify", false),
                "refillLead": st.pref_f64("refillLead").unwrap_or(engine::DEFAULT_REFILL_LEAD_MIN),
                "urgent": st.pref_bool("urgent", true),
                "urgentSupported": host.urgent_supported,
                "urgentAllowed": host.urgent_allowed,
                "autoUpdate": st.pref_bool("autoUpdate", true),
                "tired": cfg.tired_percent,
                "autostart": host.autostart,
            },
            "monitors": host.monitors,
            "taskbarLight": host.taskbar_light,
            "widgetPreview": host.widget_preview,
            "accounts": ui::accounts_json(st, self.snap.as_ref(), self.login_rechecks > 0, now),
            "inactiveProfiles": self.inactive_profiles.iter()
                .filter(|path| !self.is_fallback_profile(path))
                .filter_map(|path| path.file_name()?.to_str())
                .collect::<Vec<_>>(),
            "error": self.last_error,
            "installHelp": self.install_help,
            "loginSession": self.connecting.as_ref().map(|connection| json!({
                "id": connection.account_id.as_deref().unwrap_or(connection.provider),
                "message": connection.message(),
            })).unwrap_or_else(|| ui::login_session_json(self.login.as_ref())),
        })
    }

    fn update_json(&self) -> Value {
        let u = &self.update;
        let status = match (u.busy, &u.staged, &u.latest, &u.error) {
            (Some(b), ..) => b,
            (None, Some(_), ..) => "ready",
            // 받기·적용이 실패하면 새 버전을 알고 있어도 오류를 보여 줌 (다시 시도 단추는 화면이 띄움)
            (None, None, _, Some(_)) => "error",
            (None, None, Some(_), None) => "available",
            _ if u.checked_at.is_some() => "latest",
            _ => "idle",
        };
        json!({
            "status": status,
            "current": crate::VERSION,
            "latest": u.latest.as_ref().map(|r| r.version.clone()),
            // 새 버전 안내 (릴리스 노트, 마크다운). 너무 길면 줄 단위로 자름
            "notes": u.latest.as_ref().map(|r| crate::changelog::clip(&r.notes, 6000)),
            "page": u.latest.as_ref().map(|r| r.page.clone()),
            "checkedAt": u.checked_at.map(|t| texts::fmt_clock(t, time::now())),
            "error": u.error,
            "repo": self.platform.repo,
            "brew": self.platform.managed_update.is_some(),
            "managedHint": self.managed_hint(),
        })
    }

    // -------------------------------------------------------------- 화면 명령

    /// 설정 화면에서 온 명령 처리
    pub fn handle_ipc(&mut self, raw: &str) -> Vec<Effect> {
        let Ok(msg) = serde_json::from_str::<Value>(raw) else { return Vec::new() };
        let cmd = msg.get("cmd").and_then(|c| c.as_str()).unwrap_or("").to_string();
        if cmd == "widgetPreview" {
            self.widget_preview_open = msg["visible"].as_bool().unwrap_or(false);
            let mut effects = if self.widget_preview_open { Vec::new() } else { self.preview_widget_bar_length(None) };
            effects.push(Effect::Push);
            return effects;
        }
        if cmd == "widgetBarLength" {
            if self.platform.name == "windows" && self.widget_preview_open {
                match msg.get("value") {
                    Some(Value::Null) => return self.preview_widget_bar_length(None),
                    Some(value) => {
                        if let Some(px) = bar_pixels(value) {
                            return self.preview_widget_bar_length(Some(px));
                        }
                    }
                    None => {}
                }
            }
            return Vec::new();
        }
        let id = msg.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let provider = msg.get("provider").and_then(|p| p.as_str()).unwrap_or("").to_string();
        if cmd != "ready" && cmd != "openInstall" {
            self.last_error = None;
            self.install_help = None;
        }
        let previous = self.notices.state.clone();
        if ui::apply_meter_ipc(&mut self.notices.state, &cmd, &msg) {
            if self.notices.state.0 != previous.0 && !self.save_settings(previous) {
                return vec![Effect::Push];
            }
            if matches!(cmd.as_str(), "alertRule" | "alert" | "threshold") && self.snap.is_some() {
                self.next_rule_check = Some(time::now());
            }
            return vec![Effect::Redraw, Effect::Push];
        }
        let mut fx = Vec::new();
        match cmd.as_str() {
            "ready" => {}
            "settingsError" => {
                let message = msg["error"].as_str().unwrap_or(crate::tr!("설정 창 열기 실패", "Failed to open the settings window")).to_string();
                fx.push(Effect::Notify(crate::tr!("usAgI 설정", "usAgI settings").into(), message));
            }
            "close" => return vec![Effect::CloseSettings],
            "quit" => return vec![Effect::Quit],
            "probeNow" => self.engine.probe_now(Some(id)),
            "refresh" => self.refresh_all(),
            "resetSettings" => return self.reset_settings(),
            // 한 페이지(위젯·알림·일반)만 기본값으로
            "resetPrefs" => return self.reset_scope(msg.get("scope").and_then(|s| s.as_str()).unwrap_or("")),
            // 계정별 리필권 만료 알림: default(전체 설정) | on(이 계정 시점) | off
            "refillRule" => {
                let mode = msg.get("mode").and_then(|m| m.as_str()).unwrap_or("default");
                let lead = msg.get("lead").and_then(|l| l.as_f64()).filter(|l| l.is_finite());
                let st = &mut self.notices.state;
                let rules = st.0.entry("refill_rules").or_insert_with(|| json!({}));
                if let Some(rules) = rules.as_object_mut() {
                    match mode {
                        "on" | "off" => {
                            let lead = lead.unwrap_or(engine::DEFAULT_REFILL_LEAD_MIN).round().clamp(1.0, engine::MAX_REFILL_LEAD_MIN);
                            rules.insert(id.clone(), json!({"on": mode == "on", "lead": lead}));
                        }
                        _ => {
                            rules.remove(&id);
                        }
                    }
                }
                self.save_settings(previous);
            }
            "login" => self.login_account(&id),
            "addNew" => {
                let prefer_existing = msg["preferExisting"].as_bool().unwrap_or(false);
                match provider.as_str() {
                    "claude" => self.connect("claude", None, prefer_existing),
                    "codex" => self.connect("codex", None, prefer_existing),
                    _ => {}
                }
            }
            "openInstall" => {
                match self.install_help.as_deref() {
                    Some("claude") => fx.push(Effect::Open("https://code.claude.com/docs/en/setup#install-claude-code".into())),
                    Some("codex") => fx.push(Effect::Open("https://developers.openai.com/codex/cli".into())),
                    _ => {}
                }
            }
            "loginCode" => {
                let code = msg.get("code").and_then(|c| c.as_str()).unwrap_or("").to_string();
                if let Some(s) = self.login.as_mut() {
                    if let Err(e) = s.submit_code(&code) {
                        self.last_error = Some(e);
                    }
                }
            }
            "loginOpen" => {
                if let Some(url) = self.login.as_ref().and_then(|s| s.view().url.clone()).filter(|u| u.starts_with("https://")) {
                    fx.push(Effect::Open(url));
                }
            }
            "loginCancel" => {
                self.cancel_connection();
                self.drop_unfinished_new();
                fx.push(Effect::Redraw);
            }
            "pref" => {
                let key = msg.get("key").and_then(|k| k.as_str()).unwrap_or("").to_string();
                let value = msg.get("value").cloned().unwrap_or(Value::Null);
                fx.extend(self.set_pref(&key, value));
            }
            "account" => {
                let field = msg.get("field").and_then(|f| f.as_str()).unwrap_or("").to_string();
                let value = msg.get("value").cloned().unwrap_or(Value::Null);
                self.edit_accounts(|list| {
                    if let Some(a) = list.iter_mut().find(|a| a.id == id) {
                        match field.as_str() {
                            "show" => a.show = value.as_bool().unwrap_or(true),
                            "label" => a.label = value.as_str().unwrap_or("").trim().chars().take(24).collect(),
                            _ => {}
                        }
                    }
                    Ok(())
                });
                fx.push(Effect::Redraw);
            }
            "remove" => {
                self.remove_account(&id, accounts::save);
                fx.push(Effect::Redraw);
            }
            "removeInactiveProfile" => self.remove_inactive_profile(&id),
            "restoreInactiveProfile" => self.restore_inactive_profile(&id),
            "reorder" => {
                // 끌어서 바꾼 계정 순서 (위젯·메뉴 막대·알림 화면도 이 순서)
                let ids: Vec<String> = msg
                    .get("ids")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                if !ids.is_empty()
                    && self.edit_accounts(|list| {
                        reorder(list, &ids);
                        Ok(())
                    })
                {
                    if let Some(snap) = self.snap.as_mut() {
                        engine::sort_by_order(&mut snap.providers, &ids);
                    }
                    fx.push(Effect::Redraw);
                }
            }
            "testNotify" => fx.push(Effect::Notify(
                crate::tr!("사용량 알림", "Usage alert").into(),
                crate::tr!(
                    "알림 미리 보기 · Claude 5시간 한도의 80% 사용. 이 속도면 오후 4시 20분에 한도 도달.",
                    "Preview · Claude 5-hour limit 80% used. At this pace, limit expected at 4:20 PM."
                )
                .into(),
            )),
            "openUsage" => match provider.as_str() {
                "claude" => fx.push(Effect::Open(USAGE_URL_CLAUDE.into())),
                "codex" => fx.push(Effect::Open(USAGE_URL_CODEX.into())),
                _ => {}
            },
            "openFolder" => {
                let dir = config::config_dir();
                let _ = std::fs::create_dir_all(&dir);
                fx.push(Effect::Open(dir.to_string_lossy().into_owned()));
            }
            "openConfig" => match config::ensure_file() {
                Ok(p) => fx.push(Effect::OpenText(p)),
                Err(e) => self.last_error = Some(crate::tr!(format!("설정 파일 만들기 실패: {e}"), format!("Failed to create the config file: {e}"))),
            },
            "openNotifySettings" => fx.push(Effect::OpenNotifySettings),
            // macOS 집중 모드 설정 (허용된 앱에 usAgI 추가)
            "openFocusSettings" if self.platform.name == "mac" => {
                fx.push(Effect::Open("x-apple.systempreferences:com.apple.Focus-Settings.extension".into()))
            }
            "checkUpdate" => fx.extend(self.check_update(true)),
            "installUpdate" => fx.extend(self.install_update()),
            "openRelease" => {
                let url = self
                    .update
                    .latest
                    .as_ref()
                    .map(|r| r.page.clone())
                    .filter(|p| p.starts_with("https://github.com/"))
                    .unwrap_or_else(|| update::releases_page(self.platform.repo));
                fx.push(Effect::Open(url));
            }
            _ => {}
        }
        fx.push(Effect::Push);
        fx
    }

    /// 설정 하나 바꾸기 (값 확인 후 저장)
    pub fn set_pref(&mut self, key: &str, value: Value) -> Vec<Effect> {
        let had_bar_draft = key == "barPx" && self.widget_bar_draft.take().is_some();
        let previous = self.notices.state.clone();
        let effects = self.set_pref_inner(key, value);
        if self.notices.state.0 != previous.0 {
            if !self.save_settings(previous) {
                return if had_bar_draft { vec![Effect::WidgetBarLength, Effect::Push] } else { vec![Effect::Push] };
            }
            if matches!(key, "refresh" | "poll") {
                self.engine.refresh();
            }
            if key == "locale" {
                self.notices.state.apply_locale();
            }
        }
        effects
    }

    fn set_pref_inner(&mut self, key: &str, value: Value) -> Vec<Effect> {
        let st = &mut self.notices.state;
        let one_of = |v: &Value, allowed: &[&str]| v.as_str().filter(|s| allowed.contains(s)).map(|s| json!(s));
        match key {
            "autostart" => return vec![Effect::SetAutostart(value.as_bool().unwrap_or(false)), Effect::Push],
            "refresh" => {
                let m = value.as_f64().unwrap_or(5.0);
                let m = [1.0, 3.0, 5.0, 10.0, 15.0].into_iter().min_by(|a, b| (a - m).abs().total_cmp(&(b - m).abs())).unwrap_or(5.0);
                st.set_pref("refresh_minutes", json!(m));
            }
            "poll" => {
                st.set_pref("poll", json!(value.as_f64().unwrap_or(3.0).clamp(1.0, 60.0)));
            }
            // 위젯 막대 길이 (논리 px, 남은 시간 표시일 때의 길이). "default"·null이면 기본(예전 고정 길이)으로
            "barPx" => {
                match bar_pixels(&value) {
                    Some(v) => st.set_pref(key, json!(v)),
                    None => {
                        if let Some(p) = st.0.get_mut("prefs").and_then(|p| p.as_object_mut()) {
                            p.remove(key);
                        }
                    }
                }
                return vec![Effect::PlatformPref(key.into()), Effect::Redraw, Effect::Push];
            }
            // 언어: 시스템(기본) / 한국어 / 영어. 위젯·메뉴·알림 문구도 바뀌므로 다시 그림
            "locale" => {
                if let Some(v) = one_of(&value, &["system", "ko", "en"]) {
                    st.set_pref(key, v);
                    return vec![Effect::PlatformPref(key.into()), Effect::Redraw, Effect::Push];
                }
                return Vec::new();
            }
            // 로고 색: 자동(라이트·다크 모드를 따름) / 흰색 / 검은색. 작업 표시줄 단추·제목 표시줄·알림 아이콘
            "logo" => {
                if let Some(v) = one_of(&value, &["auto", "white", "ink"]) {
                    st.set_pref(key, v);
                    return vec![Effect::PlatformPref(key.into()), Effect::Push];
                }
                return Vec::new();
            }
            // 설정 창 테마: 기본은 시스템 설정을 따름
            "theme" => {
                if let Some(v) = one_of(&value, &["system", "light", "dark"]) {
                    st.set_pref(key, v);
                }
                return vec![Effect::Push];
            }
            "mode" => {
                if let Some(v) = one_of(&value, &["percent", "time"]) {
                    st.set_pref(key, v);
                }
            }
            "basis" => {
                // 전체 기준: "기본값"으로 둔 그래프에 모두 적용 (따로 고른 그래프는 그대로)
                if let Some(v) = one_of(&value, &["used", "left"]) {
                    st.set_pref(key, v);
                }
            }
            "tired" => {
                if let Some(v) = value.as_f64() {
                    st.set_pref(key, json!(v.clamp(1.0, 99.0)));
                }
            }
            // 리필권 만료 알림 시점 (만료 몇 분 전)
            "refillLead" => {
                if let Some(v) = value.as_f64().filter(|v| v.is_finite()) {
                    st.set_pref(key, json!(v.round().clamp(1.0, engine::MAX_REFILL_LEAD_MIN)));
                }
                return vec![Effect::Push];
            }
            "notify" | "autoUpdate" | "rabbit" | "napNotify" | "loginNotify" | "refillNotify" => {
                if let Some(v) = value.as_bool() {
                    st.set_pref(key, json!(v));
                }
            }
            // 플랫폼 전용 (Windows 위젯 위치·모니터, 중요 알림)
            "widget" | "urgent" | "bars" => {
                if let Some(v) = value.as_bool() {
                    st.set_pref(key, json!(v));
                    return vec![Effect::PlatformPref(key.into()), Effect::Redraw, Effect::Push];
                }
            }
            "side" | "monitor" => {
                let v = if key == "side" { one_of(&value, &["left", "right"]) } else { value.as_str().map(|s| json!(s)) };
                if let Some(v) = v {
                    st.set_pref(key, v);
                    return vec![Effect::PlatformPref(key.into()), Effect::Redraw, Effect::Push];
                }
            }
            _ => return Vec::new(),
        }
        vec![Effect::Redraw, Effect::Push]
    }

    // -------------------------------------------------------------- 계정·로그인

    fn edit_accounts(&mut self, f: impl FnOnce(&mut Vec<Account>) -> Result<(), String>) -> bool {
        let mut list = accounts::load(&self.notices.state);
        if let Err(e) = f(&mut list) {
            self.last_error = Some(e);
            return false;
        }
        if !self.store_accounts(&list) {
            return false;
        }
        self.engine.refresh();
        true
    }

    fn store_accounts(&mut self, list: &[Account]) -> bool {
        match accounts::save(&mut self.notices.state, list) {
            Ok(()) => true,
            Err(e) => {
                self.set_error(account_save_error(&e));
                false
            }
        }
    }

    /// 가능한 Claude 계정을 연결하고, 앱이나 프로필이 바뀌면 브리지 명령을 갱신.
    fn ensure_links(&mut self, target: Option<&str>) {
        let Ok(exe) = std::env::current_exe() else { return };
        let mut list = accounts::load(&self.notices.state);
        let mut updated = Vec::new();
        for (index, a) in list.iter_mut().enumerate() {
            if target.is_some_and(|id| a.id != id) || !accounts::needs_link(a, &exe) {
                continue;
            }
            let before = (a.chain.clone(), a.statusline_original.clone());
            if accounts::link(a, &exe).is_ok() && (a.chain.as_ref(), a.statusline_original.as_ref()) != (before.0.as_ref(), before.1.as_ref()) {
                updated.push(index);
            }
        }
        if !updated.is_empty() && !self.store_accounts(&list) {
            for index in updated {
                if let Err(error) = accounts::unlink(&mut list[index]) {
                    let prior = self.last_error.as_deref().unwrap_or_default();
                    self.set_error(format!("{prior} · {error}"));
                }
            }
        }
    }

    fn connect(&mut self, provider: &'static str, id: Option<String>, prefer_existing: bool) {
        self.cancel_connection();
        if self.pending_new.as_ref().is_some_and(|a| Some(a.id.as_str()) != id.as_deref()) {
            self.drop_unfinished_new();
        }
        match Connection::start(provider, id, &accounts::load(&self.notices.state), prefer_existing) {
            Ok(connection) => self.connecting = Some(connection),
            Err(e) => self.set_error(crate::tr!(format!("계정 연결 준비 실패: {e}"), format!("Failed to prepare the account connection: {e}"))),
        }
    }

    fn cancel_connection(&mut self) {
        self.connecting = None;
        // Signing in may have finished just before the user clicked cancel.
        // Process a successful exit before deciding which profile to discard.
        self.poll_login(time::now());
        if let Some(mut login) = self.login.take() {
            login.cancel();
        }
    }

    fn poll_connection(&mut self, effects: &mut Vec<Effect>) {
        let Some(connection) = self.connecting.as_ref() else { return };
        let Some(result) = connection.next() else { return };
        let attempt = self.connecting.take().expect("connection attempt");
        match result {
            Ok(destination) => self.finish_connection(attempt.provider, attempt.account_id.as_deref(), destination),
            Err(error) => self.set_error(error),
        }
        if self.login.is_none() {
            self.drop_unfinished_new();
        }
        effects.extend([Effect::Redraw, Effect::Push]);
    }

    fn finish_connection(&mut self, provider: &str, target: Option<&str>, destination: Destination) {
        let mut list = accounts::load(&self.notices.state);
        let program = match destination {
            Destination::Existing { profile, program } => {
                if let Some(program) = program {
                    login::remember_program(provider, program);
                }
                if list.iter().any(|account| login::same_dir(&account.path(), Some(profile.clone()))) {
                    return;
                }
                let archived_id = profile.file_name().and_then(|name| name.to_str()).filter(|name| {
                    !list.iter().any(|account| account.id == *name)
                        && self.inactive_profiles.contains(&profile)
                        && accounts::validate_managed_profile(&profile).is_ok()
                });
                let identity = login::file_status(provider, &profile).as_ref().and_then(login::Identity::from_status);
                let acc = Account {
                    id: archived_id.map(str::to_owned).unwrap_or_else(|| accounts::new_id(provider, &list)),
                    provider: provider.into(),
                    dir: profile.to_string_lossy().into(),
                    label: String::new(),
                    show: true,
                    chain: None,
                    revision: accounts::new_revision(),
                    statusline_original: None,
                    identity,
                };
                let id = acc.id.clone();
                list.push(acc);
                if !self.store_accounts(&list) {
                    return;
                }
                if archived_id.is_some() {
                    accounts::unmark_profile(&profile);
                }
                self.ensure_links(Some(&id));
                self.reload_inactive_profiles();
                self.engine.check_logins();
                self.engine.refresh();
                self.engine.probe_now(Some(id));
                return;
            }
            Destination::SignIn(program) => program,
            Destination::NeedsCli => {
                let cli = login::cli_name(provider);
                self.set_error(crate::tr!(
                    format!("{cli} 설치 필요 · 설치 후 다시 연결"),
                    format!("{cli} required · Install it, then reconnect")
                ));
                self.install_help = Some(provider.to_owned());
                return;
            }
        };
        let fresh = target.is_none();
        let acc = if let Some(id) = target {
            let Some(acc) = list.iter().chain(&self.pending_new).find(|a| a.id == id).cloned() else { return };
            acc
        } else {
            match accounts::create_profile(provider, &list) {
                Ok(acc) => acc,
                Err(e) => {
                    self.set_error(crate::tr!(format!("새 계정 폴더 만들기 실패: {e}"), format!("Failed to create the new account folder: {e}")));
                    return;
                }
            }
        };
        if accounts::is_managed(&acc) {
            let protection = accounts::validate_managed_profile(&acc.path()).and_then(|()| accounts::protect_profiles());
            if let Err(error) = protection {
                self.set_error(crate::tr!(
                    format!("프로필 권한 설정 실패 ({error})"),
                    format!("Could not protect profile folder ({error})")
                ));
                if fresh { self.delete_profile_async(acc.path()); }
                return;
            }
        }
        match LoginSession::start_with(&acc, &program) {
            Ok(session) => {
                login::remember_program(provider, program);
                self.login = Some(session);
                self.login_finished_at = None;
                self.last_login_poll = 0.0;
                if fresh {
                    self.pending_new = Some(acc);
                }
            }
            Err(e) => {
                self.set_error(crate::tr!(format!("공식 로그인 시작 실패: {e}"), format!("Failed to start the official sign-in: {e}")));
                if fresh {
                    self.delete_profile_async(acc.path());
                }
            }
        }
    }

    /// 계정 id로 공식 로그인 시작 (메뉴 막대·설정 화면의 "로그인")
    pub fn login_account(&mut self, id: &str) {
        if let Some(acc) = accounts::load(&self.notices.state).into_iter().chain(self.pending_new.clone()).find(|a| a.id == id) {
            match acc.provider.as_str() {
                "claude" => self.connect("claude", Some(id.into()), false),
                "codex" => self.connect("codex", Some(id.into()), false),
                _ => {}
            }
        }
    }

    /// 로그인 진행 상황 확인. 화면이 바뀌었으면 true.
    fn poll_login(&mut self, now: f64) -> bool {
        let Some(s) = self.login.as_mut() else { return false };
        let before = s.view().clone();
        let view = s.poll().clone();
        let identity = s.verified_status.as_ref().and_then(login::Identity::from_status);
        let mut linked = false;
        if view.done.is_some() && self.login_finished_at.is_none() {
            self.login_finished_at = Some(now);
            if view.done == Some(true) {
                if let Some(mut account) = self.pending_new.take() {
                    account.identity = identity;
                    let path = account.path();
                    if !self.edit_accounts(|accounts| {
                        accounts.push(account);
                        Ok(())
                    }) {
                        self.login = None;
                        self.reload_inactive_profiles();
                        return true;
                    }
                    accounts::unmark_profile(&path);
                    self.reload_inactive_profiles();
                } else {
                    let _guard = accounts::lock_changes();
                    let mut previous = None;
                    if !self.edit_accounts(|list| {
                        if let Some(account) = list.iter_mut().find(|a| a.id == view.account_id) {
                            previous = Some(account.clone());
                            account.revision = accounts::new_revision();
                            account.identity = identity;
                        }
                        Ok(())
                    }) {
                        self.login = None;
                        return true;
                    }
                    if let Some(account) = &previous {
                        ui::clear_usage(account);
                    }
                    if let Some(snap) = self.snap.as_mut() {
                        snap.retain_current(&accounts::load(&self.notices.state));
                    }
                }
                // 로그인한 계정의 사용량을 바로 받아 옴
                self.engine.probe_now(Some(view.account_id.clone()));
                linked = true;
            } else {
                self.drop_unfinished_new();
            }
            self.engine.check_logins();
            self.login_rechecks = LOGIN_RECHECKS;
            self.next_recheck = now + LOGIN_RECHECK_SECS;
        }
        if linked {
            self.ensure_links(Some(&view.account_id)); // 로그인 과정에서 CLI가 settings.json을 새로 썼을 수 있음
        }
        if let Some(t) = self.login_finished_at {
            if view.done == Some(true) && now - t > LOGIN_DONE_SHOW_SECS {
                self.login = None;
                return true;
            }
        }
        view != before
    }

    /// 등록까지 가지 않은 신규 로그인 프로필만 정리.
    fn drop_unfinished_new(&mut self) {
        if let Some(a) = self.pending_new.take() {
            self.delete_profile_async(a.path());
        }
    }

    fn is_fallback_profile(&self, path: &std::path::Path) -> bool {
        self.snap.as_ref().is_some_and(|snap| snap.logins.values().any(|login| {
            login.fallback.as_ref().is_some_and(|dir| login::same_dir(path, Some(PathBuf::from(dir))))
        }))
    }

    fn reload_inactive_profiles(&mut self) {
        let registered = accounts::load(&self.notices.state);
        self.inactive_profiles = accounts::managed_profiles(&accounts::profiles_dir()).into_iter().filter(|dir| {
            !registered.iter().chain(self.pending_new.iter()).any(|a| login::same_dir(dir, Some(a.path())))
                && !self.deleted_profiles.iter().any(|(path, _)| path == dir)
        }).collect();
    }

    fn remove_inactive_profile(&mut self, name: &str) {
        if self.connecting.is_some() { return; }
        self.reload_inactive_profiles();
        let Some(path) = self.inactive_profiles.iter().find(|dir| dir.file_name().and_then(|name| name.to_str()) == Some(name)).cloned()
        else { return };
        if self.is_fallback_profile(&path) {
            self.set_error(crate::tr!("다른 계정에서 사용 중인 프로필", "Profile in use by another account"));
            return;
        }
        self.delete_profile_async(path);
    }

    fn restore_inactive_profile(&mut self, name: &str) {
        if self.busy() { return; }
        self.reload_inactive_profiles();
        let Some(path) = self.inactive_profiles.iter().find(|dir| dir.file_name().and_then(|n| n.to_str()) == Some(name)).cloned()
        else { return };
        let provider = if name.starts_with("claude-") { "claude" } else { "codex" };
        match Connection::restore(provider, path) {
            Ok(connection) => self.connecting = Some(connection),
            Err(error) => self.set_error(error.to_string()),
        }
    }

    fn delete_profile_async(&mut self, path: PathBuf) {
        if let Err(e) = accounts::mark_profile_inactive(&path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                self.set_profile_delete_error(&path, &e);
            }
            self.reload_inactive_profiles();
            return;
        }
        self.queue_profile_deletion(path);
    }

    fn queue_profile_deletion(&mut self, path: PathBuf) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.deleted_profiles.push((path.clone(), rx));
        self.reload_inactive_profiles();
        std::thread::spawn(move || {
            let delete = || accounts::delete_managed_profile(&path).or_else(|e| {
                if e.kind() == std::io::ErrorKind::NotFound && !path.exists() { Ok(()) } else { Err(e) }
            });
            let mut result = delete();
            for millis in [40, 160, 500] {
                if !cfg!(windows) || !result.as_ref().is_err_and(|e| matches!(e.raw_os_error(), Some(5 | 32 | 33 | 145))) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(millis));
                result = delete();
            }
            if result.is_err() {
                let _ = accounts::mark_profile_inactive(&path);
            }
            let _ = tx.send(result);
        });
    }

    fn poll_profile_deletions(&mut self) -> bool {
        use std::sync::mpsc::TryRecvError;
        let mut error = None;
        let before = self.deleted_profiles.len();
        self.deleted_profiles.retain(|(path, result)| match result.try_recv() {
            Ok(Ok(())) => false,
            Ok(Err(e)) => { error = Some((path.clone(), e)); false }
            Err(TryRecvError::Empty) => true,
            Err(TryRecvError::Disconnected) => false,
        });
        if let Some((path, e)) = error {
            self.set_profile_delete_error(&path, &e);
        }
        if self.deleted_profiles.len() != before {
            self.reload_inactive_profiles();
            return true;
        }
        false
    }

    fn set_profile_delete_error(&mut self, path: &std::path::Path, e: &std::io::Error) {
        let path = path.display();
        self.set_error(crate::tr!(
            format!("프로필·로컬 기록 삭제 실패 ({path}: {e})"),
            format!("Could not delete the profile and local history ({path}: {e})")
        ));
    }

    fn remove_account(&mut self, id: &str, persist: impl FnOnce(&mut State, &[Account]) -> std::io::Result<()>) {
        let mut list = accounts::load(&self.notices.state);
        let Some(index) = list.iter().position(|a| a.id == id) else { return };
        let mut a = list.remove(index);
        let profile = a.path();
        let delete_profile = if accounts::is_managed(&a) && !list.iter().any(|other| login::same_dir(&profile, Some(other.path()))) {
            match accounts::validate_managed_profile(&profile) {
                Ok(()) => true,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                Err(e) => {
                    self.set_error(crate::tr!(format!("프로필 삭제 경로 확인 실패 ({e})"), format!("Could not verify the profile deletion path ({e})")));
                    return;
                }
            }
        } else { false };
        if self.connecting.as_ref().is_some_and(|c| c.account_id.as_deref() == Some(id)) {
            self.connecting = None;
        }
        if self.login.as_ref().is_some_and(|s| s.view().account_id == id) {
            if let Some(mut s) = self.login.take() {
                s.cancel();
            }
        }
        let marked = if delete_profile {
            match accounts::mark_profile_inactive(&a.path()) {
                Ok(created) => created,
                Err(e) => {
                    self.set_profile_delete_error(&a.path(), &e);
                    return;
                }
            }
        } else {
            false
        };
        let backup = if a.is_claude() && accounts::link_state(&a) == Link::Linked {
            match accounts::unlink_with_backup(&mut a) {
                Ok(backup) => backup,
                Err(e) => {
                    if marked {
                        accounts::unmark_profile(&a.path());
                    }
                    self.set_error(crate::tr!(format!("Claude 상태 표시줄 복원 실패 ({e})"), format!("Could not restore the Claude status line ({e})")));
                    return;
                }
            }
        } else {
            None
        };
        let mut updated = self.notices.state.clone();
        ui::forget_account_settings(&mut updated, &a);
        let result = {
            let _guard = accounts::lock_changes();
            persist(&mut updated, &list).map(|()| {
                self.notices.state = updated;
                ui::clear_usage(&a);
            })
        };
        if let Err(e) = result {
            if marked {
                accounts::unmark_profile(&a.path());
            }
            let mut message = account_save_error(&e);
            if let Some(Err(e)) = backup.map(|backup| backup.rollback(&a)) {
                message.push_str(&crate::tr!(format!(" · Claude 상태 표시줄 재연결 실패 ({e})"), format!(" · Could not restore the live status line ({e})")));
            }
            self.set_error(message);
            return;
        }
        if let Some(snap) = self.snap.as_mut() {
            snap.retain_current(&list);
        }
        self.engine.refresh();
        self.engine.check_logins();
        if delete_profile {
            self.queue_profile_deletion(a.path());
        } else {
            self.reload_inactive_profiles();
        }
    }

    // -------------------------------------------------------------- 업데이트

    /// 설정 초기화: 계정·로그인은 그대로, 나머지 설정과 그래프별 알림 기준을 기본값으로
    pub fn reset_settings(&mut self) -> Vec<Effect> {
        let previous = self.notices.state.clone();
        let st = &mut self.notices.state;
        st.0.remove("prefs");
        st.0.remove("alerts");
        st.0.remove("refill_rules");
        if !self.save_settings(previous) { return vec![Effect::Push]; }
        self.widget_bar_draft = None;
        crate::i18n::set(crate::i18n::resolve("system"));
        self.engine.refresh();
        // 위젯(표시·위치·모니터·막대), 로고 색, 언어를 플랫폼이 다시 맞춤
        vec![
            Effect::PlatformPref("widget".into()),
            Effect::PlatformPref("logo".into()),
            Effect::PlatformPref("locale".into()),
            Effect::Redraw,
            Effect::Push,
        ]
    }

    /// 한 페이지 설정만 기본값으로. 계정·로그인, 다른 페이지 설정, 시작 시 실행(시스템 등록)은 그대로
    /// - widget: 위젯 표시·위치·모니터·표시 값·막대 길이
    /// - alerts: 알림 켜기·끄기, 리필권 알림(계정별 포함), 그래프별 알림 기준
    /// - general: 화면 테마·언어·로고 색·갱신 주기
    pub fn reset_scope(&mut self, scope: &str) -> Vec<Effect> {
        let previous = self.notices.state.clone();
        let keys: &[&str] = match scope {
            "widget" => &["widget", "side", "monitor", "mode", "barPx", "bars"],
            "alerts" => &["notify", "napNotify", "loginNotify", "refillNotify", "refillLead", "urgent"],
            "general" => &["theme", "locale", "logo", "refresh_minutes", "poll"],
            _ => return Vec::new(),
        };
        let st = &mut self.notices.state;
        if let Some(prefs) = st.0.get_mut("prefs").and_then(|p| p.as_object_mut()) {
            for key in keys {
                prefs.remove(*key);
            }
        }
        if scope == "alerts" {
            st.0.remove("alerts");
            st.0.remove("refill_rules");
        }
        if !self.save_settings(previous) { return vec![Effect::Push]; }
        let mut fx = match scope {
            "widget" => {
                self.widget_bar_draft = None;
                vec![Effect::PlatformPref("widget".into())]
            }
            "alerts" => vec![Effect::PlatformPref("urgent".into())],
            _ => {
                crate::i18n::set(crate::i18n::resolve("system"));
                self.engine.refresh();
                vec![Effect::PlatformPref("logo".into()), Effect::PlatformPref("locale".into())]
            }
        };
        fx.extend([Effect::Redraw, Effect::Push]);
        fx
    }

    /// 패키지 관리자 업데이트 안내 문장 (명령은 플랫폼이, 문장은 지금 언어로)
    fn managed_hint(&self) -> Option<String> {
        self.platform.managed_update.map(|cmd| crate::tr!(format!("{cmd} 로 업데이트."), format!("Update with {cmd}.")))
    }

    fn check_update(&mut self, manual: bool) -> Vec<Effect> {
        if self.update.busy.is_some() {
            return Vec::new();
        }
        self.update.busy = Some("checking");
        self.update.error = None;
        vec![Effect::CheckUpdate { manual }, Effect::Push]
    }

    fn download_update(&mut self, install: bool) -> Vec<Effect> {
        let Some(release) = self.update.latest.clone() else { return Vec::new() };
        if self.update.busy.is_some() || self.platform.managed_update.is_some() {
            return Vec::new();
        }
        self.update.busy = Some("downloading");
        self.update.error = None;
        vec![Effect::DownloadUpdate { release, install }, Effect::Push]
    }

    fn install_update(&mut self) -> Vec<Effect> {
        self.update.manual_install = true;
        if self.update.staged.is_some() {
            return self.apply_staged_when_ready(true).into_iter().collect();
        }
        self.download_update(true)
    }

    fn apply_staged_when_ready(&mut self, from_button: bool) -> Option<Effect> {
        if !self.can_apply_update(from_button) {
            return None;
        }
        let path = self.update.staged.take()?;
        self.update.manual_install = false;
        self.update.busy = Some("installing");
        Some(Effect::ApplyUpdate(path))
    }

    fn can_apply_update(&self, from_button: bool) -> bool {
        self.update.busy.is_none() && self.platform.managed_update.is_none()
            && !self.busy() && (!self.settings_open || from_button)
            && (self.update.manual_install || self.auto_update())
    }

    /// 업데이트 확인 결과 (manual = 사용자가 눌러서 확인)
    pub fn update_checked(&mut self, result: Result<Option<Release>, String>, manual: bool) -> Vec<Effect> {
        self.update.busy = None;
        self.update.checked_at = Some(time::now());
        let mut fx = Vec::new();
        match result {
            Ok(Some(rel)) => {
                let ver = rel.version.clone();
                let first = self.update.latest.as_ref().map(|x| &x.version) != Some(&ver);
                self.update.latest = Some(rel);
                if self.auto_update() && self.update.staged.is_none() {
                    fx.extend(self.download_update(true));
                } else if first || manual {
                    let how = self.managed_hint().unwrap_or_else(|| crate::tr!("설정 → 정보에서 업데이트 가능.", "Update from Settings → About.").into());
                    fx.push(Effect::Notify(
                        crate::tr!("usAgI 새 버전", "usAgI update available").into(),
                        crate::tr!(format!("v{ver} 출시. {how}"), format!("v{ver} is available. {how}")),
                    ));
                }
            }
            Ok(None) => self.update.latest = None,
            Err(e) => self.update.error = Some(e),
        }
        fx.push(Effect::Push);
        fx
    }

    /// 새 버전 받은 결과
    pub fn update_downloaded(&mut self, release: Release, result: Result<PathBuf, String>, install: bool) -> Vec<Effect> {
        self.update.busy = None;
        let mut fx = Vec::new();
        match result {
            Ok(path) => {
                self.update.staged = Some(path.clone());
                self.update.latest = Some(release);
                // 설정 창을 보고 있으면 끊지 않고 기다림 (닫을 때 적용)
                if install {
                    fx.extend(self.apply_staged_when_ready(false));
                }
            }
            Err(e) => {
                self.update.manual_install = false;
                self.update.error = Some(e);
            }
        }
        fx.push(Effect::Push);
        fx
    }

    pub fn update_apply_failed(&mut self, e: String) -> Vec<Effect> {
        self.update.staged = None;
        self.update.busy = None;
        self.update.manual_install = false;
        self.update.error = Some(e);
        vec![Effect::Push]
    }
}

fn bar_pixels(value: &Value) -> Option<f64> {
    value.as_f64().filter(|v| v.is_finite()).map(|v| v.round().clamp(1.0, 400.0))
}

fn account_save_error(error: &std::io::Error) -> String {
    crate::tr!(format!("계정 설정 저장 실패 ({error})"), format!("Could not save account settings ({error})"))
}

fn storage_error(error: &std::io::Error) -> String {
    crate::tr!(format!("설정 저장 실패 ({error})"), format!("Could not save settings ({error})"))
}

/// 계정 목록을 ids 순서대로 (ids에 없는 계정은 뒤에, 원래 순서 유지)
fn reorder(list: &mut [Account], ids: &[String]) {
    list.sort_by_key(|a| ids.iter().position(|i| *i == a.id).unwrap_or(usize::MAX));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctl(name: &'static str, managed: Option<&'static str>) -> Controller {
        let mut c = Controller::new(Engine::idle(), Platform { name, repo: "o/r", managed_update: managed });
        c.notices.state.0 = serde_json::Map::new(); // 다른 테스트가 저장한 상태와 섞이지 않게
        c
    }

    /// 테스트용 계정 몇 개 (처음엔 계정이 없음)
    fn seed(c: &mut Controller, ids: &[&str]) {
        let list: Vec<Account> = ids
            .iter()
            .map(|id| Account { id: id.to_string(), provider: if id.starts_with("codex") { "codex" } else { "claude" }.into(), dir: format!("x/{id}"), label: String::new(), show: true, chain: None, revision: 0, statusline_original: None, identity: None })
            .collect();
        accounts::save(&mut c.notices.state, &list).unwrap();
    }

    #[test]
    fn threshold_edits_preserve_other_rules_and_ignore_stale_drag_targets() {
        let mut c = ctl("windows", None);
        let key = "claude-1:5시간";
        ipc(&mut c, json!({"cmd":"alert","key":key,"on":false,"at":[50,80]}));
        for (op, old, new) in [("replace",80,70), ("replace",80,60), ("add",0,80), ("delete",50,0)] {
            ipc(&mut c, json!({"cmd":"threshold","key":key,"op":op,"old":old,"new":new}));
        }
        assert_eq!(engine::alert_rule(c.state(), key), (true, vec![70.0, 80.0]));
    }

    #[test]
    fn threshold_edits_for_usage_and_reset_alerts() {
        let mut c = ctl("windows", None);
        let key = "claude-1:5시간";
        ipc(&mut c, json!({"cmd":"alert","key":key,"on":false}));
        let edits = [
            ("pct", "add", 0.0, 50.0),
            ("pct", "add", 0.0, 80.0),
            ("pct", "add", 0.0, 80.0),      // 이미 있음
            ("pct", "add", 0.0, 100.0),     // 범위 밖
            ("reset", "add", 0.0, 30.0),
            ("reset", "add", 0.0, 60.0),
            ("reset", "add", 0.0, 20000.0), // 7일 넘음
            ("reset", "replace", 60.0, 120.0),
            ("reset", "replace", 30.0, 120.0), // 이미 있는 값으로는 안 바꿈
            ("reset", "replace", 45.0, 15.0),  // 없는 값
            ("pct", "delete", 50.0, 0.0),
            ("bogus", "add", 0.0, 40.0),
        ];
        for (kind, op, old, new) in edits {
            ipc(&mut c, json!({"cmd":"threshold","key":key,"kind":kind,"op":op,"old":old,"new":new}));
        }
        let s = engine::alert_set(c.state(), key);
        assert!(s.on, "기준을 추가하면 그래프 알림 켬");
        assert_eq!((s.at, s.before), (vec![80.0], vec![30.0, 120.0]));
        // kind 없이 보내면 사용량 기준
        ipc(&mut c, json!({"cmd":"threshold","key":key,"op":"add","new":40}));
        assert_eq!(engine::alert_rule(c.state(), key), (true, vec![40.0, 80.0]));
        ipc(&mut c, json!({"cmd":"threshold","key":key,"kind":"reset","op":"delete","old":120}));
        assert_eq!(engine::alert_set(c.state(), key).before, vec![30.0]);
    }

    #[test]
    fn reset_alert_add_and_edit_respect_the_graph_window() {
        let mut c = ctl("windows", None);
        for (key, limit) in [
            ("claude-1:5시간", 300),
            ("claude-1:Fable 주간", 10080),
            ("codex-1:90분", 90),
            ("codex-1:3시간", 180),
            ("codex-1:2일", 2880),
        ] {
            ipc(&mut c, json!({"cmd":"threshold","key":key,"kind":"reset","op":"add","new":limit + 1}));
            assert!(c.state().0.get("alerts").and_then(|rules| rules.get(key)).is_none(), "{key}");
            ipc(&mut c, json!({"cmd":"threshold","key":key,"kind":"reset","op":"add","new":limit}));
            ipc(&mut c, json!({"cmd":"threshold","key":key,"kind":"reset","op":"replace","old":limit,"new":limit + 1}));
            assert_eq!(c.state().0["alerts"][key]["before"], json!([f64::from(limit)]), "{key}");
        }
    }

    #[test]
    fn alert_items_move_between_on_and_off_lists() {
        let mut c = ctl("windows", None);
        let key = "claude-1:주간";
        for (kind, new) in [("pct", 80), ("pct", 50), ("reset", 30), ("reset", 1440)] {
            ipc(&mut c, json!({"cmd":"threshold","key":key,"kind":kind,"op":"add","new":new}));
        }
        ipc(&mut c, json!({"cmd":"alertItem","key":key,"kind":"pct","value":80,"on":false}));
        ipc(&mut c, json!({"cmd":"alertItem","key":key,"kind":"reset","value":30.0,"on":false}));
        ipc(&mut c, json!({"cmd":"alertItem","key":key,"kind":"reset","value":999,"on":false})); // 없는 값
        let s = engine::alert_set(c.state(), key);
        assert_eq!((s.at.clone(), s.at_off.clone()), (vec![50.0], vec![80.0]));
        assert_eq!((s.before.clone(), s.before_off.clone()), (vec![1440.0], vec![30.0]));
        assert_eq!(c.state().0["alerts"][key]["at_off"], json!([80.0]));
        // 꺼 둔 값도 있는 값: 추가 안 함, 바꾸기·지우기는 됨
        ipc(&mut c, json!({"cmd":"threshold","key":key,"op":"add","new":80}));
        ipc(&mut c, json!({"cmd":"threshold","key":key,"op":"replace","old":80,"new":70}));
        ipc(&mut c, json!({"cmd":"threshold","key":key,"kind":"reset","op":"delete","old":30}));
        let s = engine::alert_set(c.state(), key);
        assert_eq!((s.at.clone(), s.at_off.clone()), (vec![50.0], vec![70.0]));
        assert!(s.before_off.is_empty());
        assert!(c.state().0["alerts"][key].get("before_off").is_none(), "빈 목록은 저장 안 함");
        // 다시 켜기
        ipc(&mut c, json!({"cmd":"alertItem","key":key,"kind":"pct","value":70,"on":true}));
        let s = engine::alert_set(c.state(), key);
        assert_eq!((s.at, s.at_off), (vec![50.0, 70.0], vec![]));
        // 계정 카드에 꺼 둔 항목과 리셋 임박 시점도 같이 보냄
        seed(&mut c, &["codex-1"]);
        let now = time::now();
        let snap = Snapshot {
            now,
            claude: None,
            providers: vec![engine::ProviderView {
                id: engine::ProviderId::Codex,
                account_id: "codex-1".into(),
                name: "Codex".into(),
                meters: vec![engine::Meter { label: "주간".into(), ratio: 0.4, resets_at: Some(now + 3600.0), exact: true }],
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
            }],
            logins: Default::default(),
            probes: Default::default(),
            revisions: Default::default(),
            claude_statuses: Default::default(),
        };
        let mk = "codex-1:주간";
        ipc(&mut c, json!({"cmd":"threshold","key":mk,"op":"add","new":60}));
        ipc(&mut c, json!({"cmd":"threshold","key":mk,"op":"add","new":90}));
        ipc(&mut c, json!({"cmd":"threshold","key":mk,"kind":"reset","op":"add","new":30}));
        ipc(&mut c, json!({"cmd":"threshold","key":mk,"kind":"reset","op":"add","new":60}));
        ipc(&mut c, json!({"cmd":"alertItem","key":mk,"kind":"pct","value":90,"on":false}));
        ipc(&mut c, json!({"cmd":"alertItem","key":mk,"kind":"reset","value":60,"on":false}));
        let cards = ui::accounts_json(c.state(), Some(&snap), false, now);
        let m = &cards[0]["meters"][0];
        assert_eq!((&m["alertKey"], &m["alertOn"]), (&json!(mk), &json!(true)));
        assert_eq!((&m["alertAt"], &m["alertAtOff"]), (&json!([60.0]), &json!([90.0])));
        assert_eq!((&m["before"], &m["beforeOff"]), (&json!([30.0]), &json!([60.0])));
    }

    fn ipc(c: &mut Controller, v: Value) -> Vec<Effect> {
        c.handle_ipc(&v.to_string())
    }

    #[test]
    fn widget_preview_is_requested_only_while_page_is_open() {
        let mut c = ctl("windows", None);
        assert!(!c.wants_widget_preview());
        ipc(&mut c, json!({"cmd":"widgetPreview","visible":true}));
        assert!(c.wants_widget_preview());
        ipc(&mut c, json!({"cmd":"widgetPreview","visible":false}));
        assert!(!c.wants_widget_preview());
        ipc(&mut c, json!({"cmd":"widgetPreview","visible":true}));
        c.settings_closed();
        assert!(!c.wants_widget_preview());
    }

    fn rel(v: &str) -> Release {
        Release {
            version: v.into(), page: format!("https://github.com/o/r/releases/tag/v{v}"), notes: String::new(), asset_url: "https://x/a".into(),
            verification: crate::update::Verification::Attachments { sha_url: "https://x/a.sha256".into(), sig_url: None },
        }
    }

    #[test]
    fn starts_with_no_accounts_and_no_alerts() {
        let mut c = ctl("mac", None);
        let st = c.settings_state(&HostInfo::default());
        assert_eq!(st["prefs"]["napNotify"], false, "한도 도달·리셋 알림 기본 꺼짐");
        assert_eq!(st["prefs"]["loginNotify"], true, "로그인 만료 알림 기본 켜짐");
        ipc(&mut c, json!({"cmd": "pref", "key": "napNotify", "value": true}));
        assert_eq!(c.settings_state(&HostInfo::default())["prefs"]["napNotify"], true);
        assert!(accounts::load(c.state()).is_empty(), "처음엔 연결된 계정이 없음");
        assert!(c.settings_state(&HostInfo::default())["accounts"].as_array().unwrap().is_empty());
        assert_eq!(crate::engine::alert_rule(c.state(), "claude-1:5시간"), (true, vec![]), "알림 기준도 없음");
    }

    #[test]
    fn claude_live_updates_connect_automatically_and_removal_preserves_the_profile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = json!({"model":"opus","statusLine":{"type":"command","command":"mine.sh","padding":7}});
        std::fs::write(&path, original.to_string()).unwrap();
        let local_history = dir.path().join("local-history");
        std::fs::write(&local_history, "keep").unwrap();
        let mut c = ctl("windows", None);
        seed(&mut c, &["claude-9", "codex-1"]);
        let mut list = accounts::load(c.state());
        list[0].dir = dir.path().to_string_lossy().into();
        accounts::save(&mut c.notices.state, &list).unwrap();
        c.startup();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("statusline --account claude-9"), "{text}");
        assert_eq!(accounts::load(c.state())[0].chain.as_deref(), Some("mine.sh"));
        c.startup();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let read = || serde_json::from_str::<Value>(&std::fs::read_to_string(&path).unwrap()).unwrap();
        ipc(&mut c, json!({"cmd": "remove", "id": "claude-9"}));
        assert_eq!(read(), original);
        assert_eq!(std::fs::read_to_string(&local_history).unwrap(), "keep");
        assert!(!accounts::load(c.state()).iter().any(|a| a.id == "claude-9"));
    }

    fn wait_for_cleanup(c: &mut Controller) {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !c.deleted_profiles.is_empty() && std::time::Instant::now() < end {
            c.poll_profile_deletions();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(c.deleted_profiles.is_empty());
    }

    #[test]
    fn cancel_removes_a_nonempty_pending_profile_and_saved_profiles_can_be_cleaned_up() {
        let mut c = ctl("windows", None);
        let saved = accounts::create_profile("codex", &[]).unwrap();
        let pending = accounts::create_profile("codex", std::slice::from_ref(&saved)).unwrap();
        accounts::unmark_profile(&saved.path());
        for acc in [&saved, &pending] {
            std::fs::write(acc.path().join("history.json"), "fixture").unwrap();
        }
        c.pending_new = Some(pending.clone());
        c.settings_opened();
        let listed = |c: &Controller, acc: &Account| c.settings_state(&HostInfo::default())["inactiveProfiles"]
            .as_array().unwrap().iter().any(|name| name.as_str() == Some(&acc.id));
        assert!(!listed(&c, &pending));
        assert!(listed(&c, &saved));
        assert!(accounts::profile_is_marked(&pending.path()));
        let primary = Account { dir: "fixture-primary".into(), ..saved.clone() };
        let candidates = crate::fallback::candidates(&primary, &[]);
        assert!(candidates.contains(&saved.path()));
        assert!(!candidates.contains(&pending.path()));
        ipc(&mut c, json!({"cmd":"removeInactiveProfile", "id":pending.id}));
        assert!(pending.path().exists());

        ipc(&mut c, json!({"cmd":"loginCancel"}));
        wait_for_cleanup(&mut c);
        assert!(!pending.path().exists());
        assert!(saved.path().join("history.json").exists());
        assert!(listed(&c, &saved));

        ipc(&mut c, json!({"cmd":"removeInactiveProfile", "id":saved.id}));
        wait_for_cleanup(&mut c);
        assert!(!saved.path().exists());
        assert!(!listed(&c, &saved));
    }

    #[test]
    fn profile_deletion_stops_and_restores_the_live_link_if_account_save_fails() {
        let mut c = ctl("windows", None);
        let mut a = accounts::create_profile("claude", &[]).unwrap();
        let dir = a.path();
        accounts::unmark_profile(&dir);
        std::fs::write(dir.join("history.json"), "keep").unwrap();
        let settings = dir.join("settings.json");
        std::fs::write(&settings, r#"{"statusLine":{"type":"command","command":"use-local","padding":9}}"#).unwrap();
        accounts::link(&mut a, &std::env::current_exe().unwrap()).unwrap();
        let linked = std::fs::read_to_string(&settings).unwrap();
        accounts::save(&mut c.notices.state, std::slice::from_ref(&a)).unwrap();

        c.remove_account(&a.id, |_, _| Err(std::io::ErrorKind::PermissionDenied.into()));
        assert!(c.error().is_some());
        assert!(c.deleted_profiles.is_empty());
        assert_eq!(accounts::load(c.state()), vec![a.clone()]);
        assert_eq!(std::fs::read_to_string(dir.join("history.json")).unwrap(), "keep");
        assert_eq!(std::fs::read_to_string(settings).unwrap(), linked);
        assert!(!accounts::profile_is_marked(&dir));
        ipc(&mut c, json!({"cmd": "remove", "id": a.id}));
        wait_for_cleanup(&mut c);
        assert!(accounts::load(c.state()).is_empty());
        assert!(!dir.exists());
    }

    #[test]
    fn close_and_quit_are_platform_effects() {
        let mut c = ctl("mac", None);
        assert_eq!(ipc(&mut c, json!({"cmd": "close"})), vec![Effect::CloseSettings]);
        assert_eq!(ipc(&mut c, json!({"cmd": "quit"})), vec![Effect::Quit]);
    }

    #[test]
    fn prefs_are_validated_and_saved() {
        let mut c = ctl("windows", None);
        let fx = ipc(&mut c, json!({"cmd": "pref", "key": "mode", "value": "time"}));
        assert!(fx.contains(&Effect::Redraw) && fx.contains(&Effect::Push));
        assert!(c.time_mode());
        ipc(&mut c, json!({"cmd": "pref", "key": "mode", "value": "bogus"}));
        assert!(c.time_mode(), "잘못된 값은 무시");
        ipc(&mut c, json!({"cmd": "pref", "key": "refresh", "value": 6}));
        assert_eq!(c.state().pref_f64("refresh_minutes"), Some(5.0));
        let fx = ipc(&mut c, json!({"cmd": "pref", "key": "side", "value": "left"}));
        assert!(fx.contains(&Effect::PlatformPref("side".into())));
        assert_eq!(c.state().pref_str("side", "right"), "left");
        let fx = ipc(&mut c, json!({"cmd": "pref", "key": "autostart", "value": true}));
        assert!(fx.contains(&Effect::SetAutostart(true)));
        assert!(ipc(&mut c, json!({"cmd": "pref", "key": "nope", "value": 1})) == vec![Effect::Push]);
    }

    #[test]
    fn theme_follows_the_system_by_default() {
        let mut c = ctl("windows", None);
        let host = HostInfo::default();
        assert_eq!(c.settings_state(&host)["prefs"]["theme"], "system");
        let fx = ipc(&mut c, json!({"cmd": "pref", "key": "theme", "value": "light"}));
        assert!(fx.contains(&Effect::Push) && !fx.contains(&Effect::Redraw), "위젯은 다시 그리지 않음");
        assert_eq!(c.settings_state(&host)["prefs"]["theme"], "light");
        ipc(&mut c, json!({"cmd": "pref", "key": "theme", "value": "sepia"}));
        assert_eq!(c.settings_state(&host)["prefs"]["theme"], "light", "잘못된 값은 무시");
    }

    #[test]
    fn page_reset_only_touches_that_page() {
        let mut c = ctl("windows", None);
        seed(&mut c, &["claude-1", "codex-1"]);
        ipc(&mut c, json!({"cmd": "pref", "key": "mode", "value": "time"}));
        ipc(&mut c, json!({"cmd": "pref", "key": "barPx", "value": 180}));
        ipc(&mut c, json!({"cmd": "pref", "key": "theme", "value": "dark"}));
        ipc(&mut c, json!({"cmd": "pref", "key": "napNotify", "value": true}));
        c.notices.state.0.insert("alerts".into(), json!({"claude-1:5시간": {"on": true, "at": [80]}}));
        // 위젯: 표시 값·막대 길이만
        let fx = ipc(&mut c, json!({"cmd": "resetPrefs", "scope": "widget"}));
        assert!(fx.contains(&Effect::PlatformPref("widget".into())) && fx.contains(&Effect::Push));
        assert!(!c.time_mode());
        assert_eq!(c.state().pref_f64("barPx"), None);
        assert_eq!(c.state().pref_str("theme", "system"), "dark", "일반 설정은 그대로");
        assert!(c.state().0.get("alerts").is_some(), "알림 기준은 그대로");
        // 알림: 알림 켜기·끄기와 그래프별 기준
        ipc(&mut c, json!({"cmd": "resetPrefs", "scope": "alerts"}));
        assert!(!c.state().pref_bool("napNotify", false));
        assert!(c.state().0.get("alerts").is_none());
        assert_eq!(c.state().pref_str("theme", "system"), "dark");
        // 일반: 테마
        ipc(&mut c, json!({"cmd": "resetPrefs", "scope": "general"}));
        assert_eq!(c.state().pref_str("theme", "system"), "system");
        assert_eq!(accounts::load(c.state()).len(), 2, "계정은 그대로");
        assert!(ipc(&mut c, json!({"cmd": "resetPrefs", "scope": "nope"})).is_empty());
    }

    #[test]
    fn reset_keeps_accounts_and_clears_settings() {
        let mut c = ctl("windows", None);
        seed(&mut c, &["claude-1", "codex-1"]);
        let before = accounts::load(c.state()).len();
        ipc(&mut c, json!({"cmd": "pref", "key": "mode", "value": "time"}));
        ipc(&mut c, json!({"cmd": "pref", "key": "barPx", "value": 180}));
        c.notices.state.0.insert("alerts".into(), json!({"claude-1:5시간": {"on": true, "at": [80]}}));
        let fx = ipc(&mut c, json!({"cmd": "resetSettings"}));
        assert!(fx.contains(&Effect::PlatformPref("widget".into())) && fx.contains(&Effect::Push));
        assert!(!c.time_mode());
        assert_eq!(c.state().pref_f64("barPx"), None);
        assert!(c.state().0.get("alerts").is_none());
        assert_eq!(accounts::load(c.state()).len(), before, "계정은 그대로");
    }

    #[test]
    fn refill_rules_per_account_override_the_default() {
        let mut c = ctl("windows", None);
        assert_eq!(engine::refill_rule(c.state(), "codex-1"), (false, engine::DEFAULT_REFILL_LEAD_MIN));
        ipc(&mut c, json!({"cmd": "pref", "key": "refillNotify", "value": true}));
        ipc(&mut c, json!({"cmd": "pref", "key": "refillLead", "value": 90}));
        assert_eq!(engine::refill_rule(c.state(), "codex-1"), (true, 90.0));
        ipc(&mut c, json!({"cmd": "refillRule", "id": "codex-2", "mode": "on", "lead": 30}));
        ipc(&mut c, json!({"cmd": "refillRule", "id": "codex-3", "mode": "off"}));
        assert_eq!(engine::refill_rule(c.state(), "codex-2"), (true, 30.0));
        assert!(!engine::refill_rule(c.state(), "codex-3").0);
        ipc(&mut c, json!({"cmd": "refillRule", "id": "codex-2", "mode": "default"}));
        assert_eq!(engine::refill_rule(c.state(), "codex-2"), (true, 90.0));
    }

    #[test]
    fn locale_switches_language() {
        let mut c = ctl("windows", None);
        let host = HostInfo::default();
        assert_eq!(c.settings_state(&host)["prefs"]["locale"], "system");
        let fx = ipc(&mut c, json!({"cmd": "pref", "key": "locale", "value": "en"}));
        assert!(fx.contains(&Effect::PlatformPref("locale".into())) && fx.contains(&Effect::Redraw));
        assert_eq!(c.settings_state(&host)["lang"], "en");
        assert_eq!(crate::texts::meter_label("Fable 주간"), "Fable weekly");
        ipc(&mut c, json!({"cmd": "pref", "key": "locale", "value": "ko"}));
        assert_eq!(c.settings_state(&host)["lang"], "ko");
        assert_eq!(crate::texts::meter_label("5시간"), "5시간");
        ipc(&mut c, json!({"cmd": "pref", "key": "locale", "value": "fr"}));
        assert_eq!(c.settings_state(&host)["prefs"]["locale"], "ko", "잘못된 값은 무시");
    }

    #[test]
    fn widget_bars_can_be_turned_off() {
        let mut c = ctl("windows", None);
        let host = HostInfo { widget_preview: Some(json!({"keep": true})), ..HostInfo::default() };
        assert_eq!(c.settings_state(&host)["prefs"]["bars"], true, "기본은 막대 표시");
        assert_eq!(c.settings_state(&host)["widgetPreview"]["keep"], true, "미리 보기는 그대로 전달");
        let fx = ipc(&mut c, json!({"cmd": "pref", "key": "bars", "value": false}));
        assert!(fx.contains(&Effect::PlatformPref("bars".into())) && fx.contains(&Effect::Push));
        assert_eq!(c.settings_state(&host)["prefs"]["bars"], false);
        // 위젯 페이지 초기화로 되돌아옴
        ipc(&mut c, json!({"cmd": "resetPrefs", "scope": "widget"}));
        assert_eq!(c.settings_state(&host)["prefs"]["bars"], true);
    }

    #[test]
    fn bar_length_is_pixels_with_a_default() {
        let mut c = ctl("windows", None);
        let host = HostInfo { bar_range: Some((75.0, 77.0, 240.0)), ..HostInfo::default() };
        let st = c.settings_state(&host);
        assert!(st["prefs"]["barPx"].is_null(), "설정 없음 = 기본");
        assert_eq!(st["prefs"]["barDefault"], 77.0);
        let fx = ipc(&mut c, json!({"cmd": "pref", "key": "barPx", "value": 130.4}));
        assert!(fx.contains(&Effect::PlatformPref("barPx".into())) && fx.contains(&Effect::Redraw));
        assert_eq!(c.state().pref_f64("barPx"), Some(130.0));
        ipc(&mut c, json!({"cmd": "pref", "key": "barPx", "value": null}));
        assert_eq!(c.state().pref_f64("barPx"), None, "기본값으로");
    }

    #[test]
    fn bar_length_drag_applies_before_commit_and_clears_when_settings_close() {
        let mut c = ctl("windows", None);
        c.settings_opened();
        ipc(&mut c, json!({"cmd":"widgetPreview","visible":true}));
        let drag = json!({"cmd":"widgetBarLength","value":140});
        assert_eq!(ipc(&mut c, drag.clone()), vec![Effect::WidgetBarLength]);
        assert_eq!(c.widget_bar_px(), Some(140.0));
        assert_eq!(c.state().pref_f64("barPx"), None);
        assert!(ipc(&mut c, drag).is_empty());

        ipc(&mut c, json!({"cmd":"pref","key":"barPx","value":160}));
        assert_eq!(c.widget_bar_px(), Some(160.0));
        assert_eq!(c.state().pref_f64("barPx"), Some(160.0));
        ipc(&mut c, json!({"cmd":"widgetBarLength","value":200}));
        assert_eq!(c.widget_bar_px(), Some(200.0));
        assert_eq!(c.settings_closed(), vec![Effect::WidgetBarLength]);
        assert_eq!(c.widget_bar_px(), Some(160.0));
        assert!(ipc(&mut c, json!({"cmd":"widgetBarLength","value":220})).is_empty());
    }

    #[test]
    fn logo_color_can_be_fixed() {
        let mut c = ctl("windows", None);
        let host = HostInfo::default();
        assert_eq!(c.settings_state(&host)["prefs"]["logo"], "auto");
        let fx = ipc(&mut c, json!({"cmd": "pref", "key": "logo", "value": "ink"}));
        assert!(fx.contains(&Effect::PlatformPref("logo".into())), "알림 아이콘을 다시 쓰게 플랫폼에 알림");
        assert_eq!(c.settings_state(&host)["prefs"]["logo"], "ink");
        ipc(&mut c, json!({"cmd": "pref", "key": "logo", "value": "gold"}));
        assert_eq!(c.settings_state(&host)["prefs"]["logo"], "ink", "잘못된 값은 무시");
    }

    #[test]
    fn toggle_mode_flips() {
        let mut c = ctl("mac", None);
        c.toggle_mode();
        assert!(c.time_mode());
        c.toggle_mode();
        assert!(!c.time_mode());
    }

    #[test]
    fn usage_pages_are_fixed_urls() {
        let mut c = ctl("mac", None);
        assert!(ipc(&mut c, json!({"cmd": "openUsage", "provider": "claude"})).contains(&Effect::Open(USAGE_URL_CLAUDE.into())));
        assert!(ipc(&mut c, json!({"cmd": "openUsage", "provider": "codex"})).contains(&Effect::Open(USAGE_URL_CODEX.into())));
        assert!(!ipc(&mut c, json!({"cmd": "openUsage", "provider": "file:///etc"})).iter().any(|e| matches!(e, Effect::Open(_))));
    }

    #[test]
    fn invalid_provider_does_not_start_a_connection() {
        let mut c = ctl("mac", None);
        ipc(&mut c, json!({"cmd":"addNew","provider":"other"}));
        assert!(!c.busy());
        assert!(accounts::load(c.state()).is_empty());
    }

    #[test]
    fn new_sign_in_uses_a_separate_profile_even_with_a_valid_default_login() {
        const FIXTURE: &str = "USAGI_TEST_NEW_ACCOUNT_CLI";
        if let Some(program) = std::env::var_os(FIXTURE) {
            login::remember_program("codex", PathBuf::from(program));
            let default = accounts::default_codex_dir().unwrap();
            let mut c = ctl("test", None);
            let wait_for_accounts = |c: &mut Controller, count| {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
                loop {
                    c.tick();
                    let accounts = accounts::load(c.state());
                    if accounts.len() == count {
                        return accounts;
                    }
                    assert!(
                        c.error().is_none() && std::time::Instant::now() < deadline,
                        "connection error: {:?}, login: {}",
                        c.error(),
                        c.settings_state(&HostInfo::default())["login"],
                    );
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            };

            ipc(&mut c, json!({"cmd":"addNew","provider":"codex"}));
            let added = wait_for_accounts(&mut c, 1);
            assert!(accounts::is_managed(&added[0]));
            let new_login = login::file_status("codex", &added[0].path()).unwrap();
            assert_eq!(new_login.organization_id.as_deref(), Some("new-account"));

            ipc(&mut c, json!({"cmd":"addNew","provider":"codex","preferExisting":true}));
            let added = wait_for_accounts(&mut c, 2);
            assert!(login::same_dir(&added[1].path(), Some(default)));
            return;
        }

        let fixture = tempfile::tempdir().unwrap();
        let default = fixture.path().join("default");
        std::fs::create_dir(&default).unwrap();
        let new_auth = fixture.path().join("new-account.json");
        let claims = format!(r#"{{"exp":{:.0}}}"#, time::now() + 3600.0);
        let payload = crate::b64::encode(claims.as_bytes());
        let token = format!("header.{}.signature", payload.trim_end_matches('='));
        for (auth, workspace) in [(default.join("auth.json"), "old-account"), (new_auth.clone(), "new-account")] {
            std::fs::write(auth, json!({"auth_mode":"chatgpt","tokens":{"access_token":token,"account_id":workspace}}).to_string()).unwrap();
        }
        #[cfg(windows)]
        let (filename, script) = ("codex.cmd", concat!(
            "@echo off\r\n",
            "if \"%1\"==\"--version\" (echo codex-cli fixture& exit /b 0)\r\n",
            "if \"%1\"==\"login\" copy /y \"%USAGI_TEST_NEW_ACCOUNT_AUTH%\" \"%CODEX_HOME%\\auth.json\" >nul\r\n",
        ));
        #[cfg(unix)]
        let (filename, script) = ("codex", concat!(
            "#!/bin/sh\n",
            "if [ \"$1\" = --version ]; then echo 'codex-cli fixture'; exit 0; fi\n",
            "[ \"$1\" = login ] && cp \"$USAGI_TEST_NEW_ACCOUNT_AUTH\" \"$CODEX_HOME/auth.json\"\n",
        ));
        let program = fixture.path().join(filename);
        std::fs::write(&program, script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "controller::tests::new_sign_in_uses_a_separate_profile_even_with_a_valid_default_login", "--nocapture"])
            .env(FIXTURE, program)
            .env("CODEX_HOME", default)
            .env("USAGI_TEST_NEW_ACCOUNT_AUTH", new_auth)
            .output()
            .unwrap();
        assert!(result.status.success(), "{}\n{}", String::from_utf8_lossy(&result.stdout), String::from_utf8_lossy(&result.stderr));
    }

    #[test]
    fn remove_drops_account_and_its_settings() {
        let mut c = ctl("windows", None);
        seed(&mut c, &["claude-1", "codex-1"]);
        let list = accounts::load(c.state());
        let id = list[0].id.clone();
        ipc(&mut c, json!({"cmd": "alert", "key": format!("{id}:5시간"), "on": true, "at": [70]}));
        ipc(&mut c, json!({"cmd": "meterBasis", "key": format!("{id}:5시간"), "value": "left"}));
        let fx = ipc(&mut c, json!({"cmd": "remove", "id": id}));
        assert!(fx.contains(&Effect::Redraw));
        assert!(!accounts::load(c.state()).iter().any(|a| a.id == id));
        let alerts = c.state().0.get("alerts").and_then(|v| v.as_object()).map(|m| m.len()).unwrap_or(0);
        assert_eq!(alerts, 0, "지운 계정의 알림 기준도 정리");
    }

    #[test]
    fn meter_basis_default_removes_override() {
        let mut c = ctl("mac", None);
        ipc(&mut c, json!({"cmd": "meterBasis", "key": "a:5시간", "value": "left"}));
        assert!(ui::meter_left(c.state(), "a:5시간"));
        ipc(&mut c, json!({"cmd": "meterBasis", "key": "a:5시간", "value": "default"}));
        assert!(!ui::meter_left(c.state(), "a:5시간"));
        ipc(&mut c, json!({"cmd": "alert", "key": "a:5시간", "at": 150}));
        assert_eq!(c.state().0["alerts"]["a:5시간"]["at"], json!([99.0]), "하나만 보내도, 범위 안으로");
    }

    #[test]
    fn same_state_shape_on_every_platform() {
        let win = ctl("windows", None).settings_state(&HostInfo { autostart: true, urgent_supported: true, urgent_allowed: Some(false), monitors: vec![json!({"id": "D1", "label": "모니터 1"})], taskbar_light: Some(true), bar_range: None, widget_preview: None });
        assert_eq!(win["taskbarLight"], true);
        let mac = ctl("mac", None).settings_state(&HostInfo::default());
        assert_eq!(win["platform"], "windows");
        assert_eq!(mac["platform"], "mac");
        let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
        assert_eq!(keys(&win), keys(&mac));
        assert_eq!(keys(&win["prefs"]), keys(&mac["prefs"]));
        assert_eq!(win["prefs"]["autostart"], true);
        assert_eq!(win["prefs"]["urgentAllowed"], false);
        assert_eq!(mac["prefs"]["urgentAllowed"], Value::Null);
        assert_eq!(win["monitors"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn schedules_controller_ticks_only_for_pending_work() {
        let mut c = ctl("mac", None);
        let now = 100.0;
        let seconds = |n| Some(std::time::Duration::from_secs(n));
        c.next_update_check = now + 60.0;
        assert_eq!(c.next_tick_delay(now), seconds(60));

        c.notices.state.set_pref("updateCheck", json!(false));
        assert_eq!(c.next_tick_delay(now), None);

        c.login_rechecks = 1;
        c.next_recheck = now + 3.0;
        assert_eq!(c.next_tick_delay(now), seconds(3));
        let (_sender, receiver) = std::sync::mpsc::channel();
        c.deleted_profiles.push((PathBuf::from("deleted"), receiver));
        assert_eq!(c.next_tick_delay(now), seconds(1));
        c.deleted_profiles.clear();
        c.login_rechecks = 0;

        c.update.staged = Some(PathBuf::from("new"));
        c.settings_opened();
        assert_eq!(c.next_tick_delay(now), None);
        c.settings_open = false;
        assert_eq!(c.next_tick_delay(now), seconds(0));
    }

    #[test]
    fn auto_update_downloads_then_waits_for_settings_to_close() {
        let mut c = ctl("windows", None);
        let fx = c.update_checked(Ok(Some(rel("9.0.0"))), false);
        assert!(fx.iter().any(|e| matches!(e, Effect::DownloadUpdate { install: true, .. })));
        c.settings_opened();
        let fx = c.update_downloaded(rel("9.0.0"), Ok(PathBuf::from("new")), true);
        assert!(!fx.iter().any(|e| matches!(e, Effect::ApplyUpdate(_))), "설정 창을 보는 중엔 기다림");
        assert_eq!(c.settings_state(&HostInfo::default())["update"]["status"], "ready");
        assert_eq!(c.settings_closed(), vec![Effect::ApplyUpdate(PathBuf::from("new"))]);
        c.update_apply_failed("x".into());
        assert_eq!(c.settings_state(&HostInfo::default())["update"]["status"], "error");
    }

    #[test]
    fn auto_update_respects_changes_after_download_started() {
        let mut c = ctl("windows", None);
        c.update_checked(Ok(Some(rel("9.0.0"))), false);
        c.set_pref("autoUpdate", json!(false));
        let effects = c.update_downloaded(rel("9.0.0"), Ok(PathBuf::from("new")), true);
        assert!(!effects.iter().any(|e| matches!(e, Effect::ApplyUpdate(_))));
        let effects = ipc(&mut c, json!({"cmd": "installUpdate"}));
        assert!(effects.contains(&Effect::ApplyUpdate(PathBuf::from("new"))));
    }

    #[test]
    fn managed_install_only_notifies() {
        let mut c = ctl("mac", Some("brew upgrade --cask usagi"));
        let fx = c.update_checked(Ok(Some(rel("9.0.0"))), true);
        assert!(!fx.iter().any(|e| matches!(e, Effect::DownloadUpdate { .. })));
        assert!(fx.iter().any(|e| matches!(e, Effect::Notify(_, m) if m.contains("brew upgrade"))));
        assert!(ipc(&mut c, json!({"cmd": "installUpdate"})).iter().all(|e| !matches!(e, Effect::DownloadUpdate { .. })));
        let st = c.settings_state(&HostInfo::default());
        assert_eq!(st["update"]["brew"], true);
        assert_eq!(st["update"]["status"], "available");
    }

    #[test]
    fn release_page_falls_back_to_list() {
        let mut c = ctl("mac", None);
        assert!(ipc(&mut c, json!({"cmd": "openRelease"})).contains(&Effect::Open("https://github.com/o/r/releases".into())));
    }

    #[test]
    fn reorder_moves_accounts_and_keeps_unknown_last() {
        let mut c = ctl("windows", None);
        seed(&mut c, &["claude-1", "codex-1", "codex-9"]);
        let ids: Vec<String> = accounts::load(c.state()).iter().map(|a| a.id.clone()).collect();
        let last = ids.last().unwrap().clone();
        let fx = ipc(&mut c, json!({"cmd": "reorder", "ids": [last.clone()]}));
        assert!(fx.contains(&Effect::Redraw));
        let after: Vec<String> = accounts::load(c.state()).iter().map(|a| a.id.clone()).collect();
        assert_eq!(after[0], last, "맨 위로");
        let mut rest = ids.clone();
        rest.retain(|x| *x != last);
        assert_eq!(&after[1..], &rest[..], "나머지는 원래 순서");
        assert_eq!(after.len(), ids.len(), "계정이 없어지지 않음");
    }
}
