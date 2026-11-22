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
}

#[derive(Default)]
struct UpdateState {
    busy: Option<&'static str>,
    latest: Option<Release>,
    staged: Option<PathBuf>,
    checked_at: Option<f64>,
    error: Option<String>,
}

pub struct Controller {
    pub engine: Engine,
    pub notices: Notices,
    pub snap: Option<Snapshot>,
    pub platform: Platform,
    last_error: Option<String>,
    /// 콘솔 없이 돌고 있는 공식 로그인 (한 번에 하나)
    login: Option<LoginSession>,
    connecting: Option<Connection>,
    /// 마지막으로 화면에 보낸 연결 준비 문구
    connect_message: String,
    login_finished_at: Option<f64>,
    last_login_poll: f64,
    login_rechecks: u32,
    next_recheck: f64,
    /// "추가"로 만들어 공식 로그인 중인, 아직 등록 전인 프로필
    pending_new: Option<Account>,
    update: UpdateState,
    next_update_check: f64,
    settings_open: bool,
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
            login: None,
            connecting: None,
            connect_message: String::new(),
            login_finished_at: None,
            last_login_poll: 0.0,
            login_rechecks: 0,
            next_recheck: 0.0,
            pending_new: None,
            update: UpdateState::default(),
            next_update_check: time::now() + UPDATE_FIRST_SECS,
            settings_open: false,
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
        self.ensure_links();
        self.drop_legacy_accounts();
        std::thread::spawn(|| {
            remove_legacy_data();
            accounts::protect_profiles();
        });
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
    pub fn apply_snapshot(&mut self, snap: Snapshot) -> Vec<Effect> {
        let cfg = self.effective_config();
        let mut fx: Vec<Effect> = self.notices.check(self.snap.as_ref(), &snap, &cfg).into_iter().map(|(t, m)| Effect::Notify(t, m)).collect();
        self.snap = Some(snap);
        fx.push(Effect::Redraw);
        fx.push(Effect::Push);
        fx
    }

    /// 자주 불러 주세요 (0.2초 안팎). 로그인 진행 확인, 로그인 후 재확인, 업데이트 확인 시각을 스스로 셈.
    pub fn tick(&mut self) -> Vec<Effect> {
        let now = time::now();
        let mut fx = Vec::new();
        self.poll_connection(&mut fx);
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
        fx
    }

    /// 로그인 진행을 보고 있어 tick을 자주(0.2초쯤) 불러야 하는지. 아니면 1초에 한 번이면 충분해요.
    pub fn busy(&self) -> bool {
        self.login.is_some() || self.connecting.is_some()
    }

    pub fn settings_opened(&mut self) {
        self.settings_open = true;
        self.engine.check_logins();
    }

    /// 설정 창을 닫음 → 받아 둔 업데이트가 있으면 이제 적용
    pub fn settings_closed(&mut self) -> Vec<Effect> {
        self.settings_open = false;
        match &self.update.staged {
            Some(p) if self.auto_update() => vec![Effect::ApplyUpdate(p.clone())],
            _ => Vec::new(),
        }
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
                "basis": st.pref_str("basis", "used"),
                "refresh": engine::refresh_minutes(st),
                "rabbit": st.pref_bool("rabbit", true),
                "notify": cfg.notify,
                "napNotify": st.pref_bool("napNotify", false),
                "loginNotify": st.pref_bool("loginNotify", false),
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
            "accounts": ui::accounts_json(st, self.snap.as_ref(), self.login_rechecks > 0, now),
            "error": self.last_error,
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
            "notes": u.latest.as_ref().map(|r| r.notes.chars().take(1200).collect::<String>()),
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
        let id = msg.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let provider = msg.get("provider").and_then(|p| p.as_str()).unwrap_or("").to_string();
        if cmd != "ready" {
            self.last_error = None;
        }
        if ui::apply_meter_ipc(&mut self.notices.state, &cmd, &msg) {
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
                st.save();
            }
            "login" => self.login_account(&id),
            "addNew" => match provider.as_str() {
                "claude" => self.connect("claude", None),
                "codex" => self.connect("codex", None),
                _ => {}
            },
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
                self.remove_account(&id);
                fx.push(Effect::Redraw);
            }
            "reorder" => {
                // 끌어서 바꾼 계정 순서 (위젯·메뉴 막대·알림 화면도 이 순서)
                let ids: Vec<String> = msg
                    .get("ids")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                    .unwrap_or_default();
                if !ids.is_empty() {
                    self.edit_accounts(|list| {
                        reorder(list, &ids);
                        Ok(())
                    });
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
        let st = &mut self.notices.state;
        let one_of = |v: &Value, allowed: &[&str]| v.as_str().filter(|s| allowed.contains(s)).map(|s| json!(s));
        match key {
            "autostart" => return vec![Effect::SetAutostart(value.as_bool().unwrap_or(false)), Effect::Push],
            "refresh" => {
                let m = value.as_f64().unwrap_or(5.0);
                let m = [1.0, 3.0, 5.0, 10.0, 15.0].into_iter().min_by(|a, b| (a - m).abs().total_cmp(&(b - m).abs())).unwrap_or(5.0);
                st.set_pref("refresh_minutes", json!(m));
                self.engine.refresh();
            }
            "poll" => {
                st.set_pref("poll", json!(value.as_f64().unwrap_or(3.0).clamp(1.0, 60.0)));
                self.engine.refresh();
            }
            // 위젯 막대 길이 (논리 px, 남은 시간 표시일 때의 길이). "default"·null이면 기본(예전 고정 길이)으로
            "barPx" => {
                match value.as_f64().filter(|v| v.is_finite()) {
                    Some(v) => st.set_pref(key, json!(v.round().clamp(1.0, 400.0))),
                    None => {
                        if let Some(p) = st.0.get_mut("prefs").and_then(|p| p.as_object_mut()) {
                            p.remove(key);
                        }
                        st.save();
                    }
                }
                return vec![Effect::PlatformPref(key.into()), Effect::Redraw, Effect::Push];
            }
            // 언어: 시스템(기본) / 한국어 / 영어. 위젯·메뉴·알림 문구도 바뀌므로 다시 그림
            "locale" => {
                if let Some(v) = one_of(&value, &["system", "ko", "en"]) {
                    crate::i18n::set(crate::i18n::resolve(v.as_str().unwrap_or("system")));
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
            "widget" | "urgent" => {
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

    fn edit_accounts(&mut self, f: impl FnOnce(&mut Vec<Account>) -> Result<(), String>) {
        let mut list = accounts::load(&self.notices.state);
        if let Err(e) = f(&mut list) {
            self.last_error = Some(e);
        }
        accounts::save(&mut self.notices.state, &list);
        self.engine.refresh();
    }

    /// Claude 계정은 기본으로 실시간 연결: Claude Code 상태 표시줄에 usAgI 브리지를 걸어 메시지마다 사용량이 갱신되게 함.
    /// 이미 걸려 있으면 아무것도 하지 않고, 앱 위치가 바뀌었으면 새 위치로 다시 검.
    /// 원래 쓰던 상태 표시줄은 기억해 두고 그 출력을 그대로 보여 줌 (계정을 지울 때 되돌림).
    fn ensure_links(&mut self) {
        let Ok(exe) = std::env::current_exe() else { return };
        let mut list = accounts::load(&self.notices.state);
        let mut changed = false;
        for a in list.iter_mut() {
            if accounts::needs_link(a, &exe) {
                let before = a.chain.clone();
                if accounts::link(a, &exe).is_ok() {
                    changed |= a.chain != before;
                }
            }
        }
        if changed {
            accounts::save(&mut self.notices.state, &list);
        }
    }

    /// 예전 브라우저 연결 계정(프로필 폴더 없음)을 저장된 목록에서도 지움
    fn drop_legacy_accounts(&mut self) {
        let stored = self.notices.state.0.get("accounts").and_then(|v| v.as_array()).map_or(0, Vec::len);
        let list = accounts::load(&self.notices.state);
        if list.len() != stored {
            accounts::save(&mut self.notices.state, &list);
        }
    }

    fn connect(&mut self, provider: &'static str, id: Option<String>) {
        self.cancel_connection();
        if self.pending_new.as_ref().is_some_and(|a| Some(a.id.as_str()) != id.as_deref()) {
            self.drop_unfinished_new();
        }
        match Connection::start(provider, id, &accounts::load(&self.notices.state)) {
            Ok(connection) => self.connecting = Some(connection),
            Err(e) => self.set_error(crate::tr!(format!("계정 연결 준비 실패: {e}"), format!("Failed to prepare the account connection: {e}"))),
        }
    }

    fn cancel_connection(&mut self) {
        self.connecting = None;
        if let Some(mut login) = self.login.take() {
            login.cancel();
        }
    }

    fn poll_connection(&mut self, effects: &mut Vec<Effect>) {
        let Some(connection) = self.connecting.as_ref() else { return };
        let Some(result) = connection.next() else {
            // 설치가 시작되면 화면 문구도 바꿈
            let message = connection.message();
            if message != self.connect_message {
                self.connect_message = message;
                effects.push(Effect::Push);
            }
            return;
        };
        let attempt = self.connecting.take().expect("connection attempt");
        self.connect_message.clear();
        match result {
            Ok(destination) => self.finish_connection(attempt.provider, attempt.account_id.as_deref(), destination),
            Err(error) => self.set_error(error),
        }
        effects.extend([Effect::Redraw, Effect::Push]);
    }

    fn finish_connection(&mut self, provider: &str, target: Option<&str>, destination: Destination) {
        let mut list = accounts::load(&self.notices.state);
        let program = match destination {
            Destination::Existing { profile, program } => {
                login::remember_program(provider, program);
                let acc = Account {
                    id: accounts::new_id(provider, &list),
                    provider: provider.into(),
                    dir: profile.to_string_lossy().into(),
                    label: String::new(),
                    show: true,
                    chain: None,
                };
                let id = acc.id.clone();
                list.push(acc);
                accounts::save(&mut self.notices.state, &list);
                self.ensure_links();
                self.engine.check_logins();
                self.engine.refresh();
                self.engine.probe_now(Some(id));
                return;
            }
            Destination::SignIn(program) => program,
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
                    let _ = std::fs::remove_dir(acc.path());
                }
            }
        }
    }

    /// 계정 id로 공식 로그인 시작 (메뉴 막대·설정 화면의 "로그인")
    pub fn login_account(&mut self, id: &str) {
        if let Some(acc) = accounts::load(&self.notices.state).into_iter().chain(self.pending_new.clone()).find(|a| a.id == id) {
            match acc.provider.as_str() {
                "claude" => self.connect("claude", Some(id.into())),
                "codex" => self.connect("codex", Some(id.into())),
                _ => {}
            }
        }
    }

    /// 로그인 진행 상황 확인. 화면이 바뀌었으면 true.
    fn poll_login(&mut self, now: f64) -> bool {
        let Some(s) = self.login.as_mut() else { return false };
        let before = s.view().clone();
        let view = s.poll().clone();
        let mut linked = false;
        if view.done.is_some() && self.login_finished_at.is_none() {
            self.login_finished_at = Some(now);
            if view.done == Some(true) {
                if let Some(account) = self.pending_new.take() {
                    self.edit_accounts(|accounts| {
                        accounts.push(account);
                        Ok(())
                    });
                }
                // 로그인한 계정의 사용량을 바로 받아 옴 (Codex는 토큰을 쓰지 않음)
                self.engine.probe_now(Some(view.account_id.clone()));
                linked = true;
            }
            self.engine.check_logins();
            self.login_rechecks = LOGIN_RECHECKS;
            self.next_recheck = now + LOGIN_RECHECK_SECS;
        }
        if linked {
            self.ensure_links(); // 로그인 과정에서 CLI가 settings.json을 새로 썼을 수 있음
        }
        if let Some(t) = self.login_finished_at {
            if view.done == Some(true) && now - t > LOGIN_DONE_SHOW_SECS {
                self.login = None;
                return true;
            }
        }
        view != before
    }

    /// "추가"로 만든 계정을 로그인하지 않은 채 닫으면 지움 (빈 카드가 남지 않게)
    fn drop_unfinished_new(&mut self) {
        if let Some(a) = self.pending_new.take() {
            ui::forget_account(&mut self.notices.state, &a);
        }
    }

    fn remove_account(&mut self, id: &str) {
        if self.connecting.as_ref().is_some_and(|c| c.account_id.as_deref() == Some(id)) {
            self.connecting = None;
        }
        if self.login.as_ref().is_some_and(|s| s.view().account_id == id) {
            if let Some(mut s) = self.login.take() {
                s.cancel();
            }
        }
        if self.pending_new.as_ref().is_some_and(|a| a.id == id) {
            self.drop_unfinished_new();
            return;
        }
        let mut removed: Option<Account> = None;
        self.edit_accounts(|list| {
            if let Some(i) = list.iter().position(|a| a.id == id) {
                let mut a = list.remove(i);
                if a.is_claude() && accounts::link_state(&a) == Link::Linked {
                    let _ = accounts::unlink(&mut a);
                }
                removed = Some(a);
            }
            Ok(())
        });
        if let Some(a) = removed {
            ui::forget_account(&mut self.notices.state, &a);
        }
    }

    // -------------------------------------------------------------- 업데이트

    /// 설정 초기화: 계정·로그인은 그대로, 나머지 설정과 그래프별 알림 기준을 기본값으로
    pub fn reset_settings(&mut self) -> Vec<Effect> {
        let st = &mut self.notices.state;
        st.0.remove("prefs");
        st.0.remove("alerts");
        st.0.remove("refill_rules");
        st.save();
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
        let keys: &[&str] = match scope {
            "widget" => &["widget", "side", "monitor", "mode", "barPx"],
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
        st.save();
        let mut fx = match scope {
            "widget" => vec![Effect::PlatformPref("widget".into())],
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
        match &self.update.staged {
            Some(p) => vec![Effect::ApplyUpdate(p.clone())],
            None => self.download_update(true),
        }
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
                if self.auto_update() {
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
                if install && !self.settings_open {
                    fx.push(Effect::ApplyUpdate(path));
                }
            }
            Err(e) => self.update.error = Some(e),
        }
        fx.push(Effect::Push);
        fx
    }

    pub fn update_apply_failed(&mut self, e: String) -> Vec<Effect> {
        self.update.staged = None;
        self.update.error = Some(e);
        vec![Effect::Push]
    }
}

/// 더 쓰지 않는 예전 데이터 정리: 브라우저 연결용 프로필·사용량 파일, 예전 설정 창(WebView2) 데이터
fn remove_legacy_data() {
    let dir = config::config_dir();
    for name in ["browser-profiles", "webview"] {
        let _ = std::fs::remove_dir_all(dir.join(name));
    }
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("browser-usage-") && name.ends_with(".json") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
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
            .map(|id| Account { id: id.to_string(), provider: if id.starts_with("codex") { "codex" } else { "claude" }.into(), dir: format!("x/{id}"), label: String::new(), show: true, chain: None })
            .collect();
        accounts::save(&mut c.notices.state, &list);
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
                plan: None,
                credits: None,
                refills: None,
                extra: None,
            }],
            logins: Default::default(),
            probes: Default::default(),
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

    fn rel(v: &str) -> Release {
        Release { version: v.into(), page: format!("https://github.com/o/r/releases/tag/v{v}"), notes: String::new(), asset_url: "https://x/a".into(), sha_url: "https://x/a.sha256".into(), sig_url: None }
    }

    #[test]
    fn starts_with_no_accounts_and_no_alerts() {
        let mut c = ctl("mac", None);
        let st = c.settings_state(&HostInfo::default());
        assert_eq!(st["prefs"]["napNotify"], false, "한도 도달·리셋 알림 기본 꺼짐");
        assert_eq!(st["prefs"]["loginNotify"], false, "로그인 만료 알림 기본 꺼짐");
        ipc(&mut c, json!({"cmd": "pref", "key": "napNotify", "value": true}));
        assert_eq!(c.settings_state(&HostInfo::default())["prefs"]["napNotify"], true);
        assert!(accounts::load(c.state()).is_empty(), "처음엔 연결된 계정이 없음");
        assert!(c.settings_state(&HostInfo::default())["accounts"].as_array().unwrap().is_empty());
        assert_eq!(crate::engine::alert_rule(c.state(), "claude-1:5시간"), (true, vec![]), "알림 기준도 없음");
    }

    #[test]
    fn claude_accounts_are_linked_by_default() {
        let dir = std::env::temp_dir().join(format!("usagi-autolink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.json"), r#"{"model":"opus","statusLine":{"type":"command","command":"mine.sh"}}"#).unwrap();
        let mut c = ctl("windows", None);
        // 실제 폴더가 있는 Claude 계정 하나, 폴더가 없는 가짜 계정들
        seed(&mut c, &["claude-9", "codex-1"]);
        let mut list = accounts::load(c.state());
        list[0].dir = dir.to_string_lossy().into();
        accounts::save(&mut c.notices.state, &list);
        c.startup();
        let text = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(text.contains("statusline --account claude-9"), "{text}");
        assert!(text.contains("\"model\""), "다른 설정은 그대로");
        let acc = accounts::load(c.state()).into_iter().find(|a| a.id == "claude-9").unwrap();
        assert_eq!(acc.chain.as_deref(), Some("mine.sh"), "원래 상태 표시줄은 기억");
        // 화면에서 보내는 연결·해제 명령은 더는 없음
        let before = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        ipc(&mut c, json!({"cmd": "unlink", "id": "claude-9"}));
        assert_eq!(std::fs::read_to_string(dir.join("settings.json")).unwrap(), before);
        // 계정을 지우면 원래 상태 표시줄로 되돌림
        ipc(&mut c, json!({"cmd": "remove", "id": "claude-9"}));
        let text = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(text.contains("mine.sh") && !text.contains("statusline --account"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
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
        let win = ctl("windows", None).settings_state(&HostInfo { autostart: true, urgent_supported: true, urgent_allowed: Some(false), monitors: vec![json!({"id": "D1", "label": "모니터 1"})], taskbar_light: Some(true), bar_range: None });
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
