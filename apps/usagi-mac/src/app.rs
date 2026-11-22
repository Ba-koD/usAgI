//! macOS 메뉴 막대 앱.
//!
//! - 메뉴 막대: [로고] 5h 53%  7d 64%  │  [로고] 7d 92% (토끼 없이, 공급자 로고만)
//! - 누르면: 설정 열기, 계정별 사용량·리셋까지 남은 시간, 로그인, 새로고침, 보기 바꾸기, 종료
//! - 설정 창: Windows와 같은 화면(settings.html), 화면 상태와 명령 처리도 Windows와 같은
//!   usagi-core 컨트롤러. 이 파일은 메뉴 막대·창·알림·로그인할 때 실행·업데이트 적용만 맡아요.

use crate::settings::SettingsWindow;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS, EventLoopWindowTargetExtMacOS};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};
use usagi_core::accounts::{self, Account};
use usagi_core::config;
use usagi_core::controller::{Controller, Effect, HostInfo, Platform};
use usagi_core::engine::{alert_key, Engine, Meter, ProviderView, Snapshot};
use usagi_core::update::Release;
use usagi_core::{codex, texts, time, ui};

/// 로그인 진행을 보고 있을 때의 확인 간격
const FRAME: Duration = Duration::from_millis(200);
/// 평소 간격 (할 일은 로그인 후 재확인·업데이트 확인 시각뿐이라 드물게 깨어나도 됨)
const IDLE_FRAME: Duration = Duration::from_secs(1);

pub enum UserEvent {
    Snapshot,
    Menu(MenuEvent),
    Ipc(String),
    Update(UpdateEvent),
}

pub enum UpdateEvent {
    Checked(Result<Option<Release>, String>, bool),
    Downloaded(Release, Result<std::path::PathBuf, String>, bool),
}

struct App {
    proxy: EventLoopProxy<UserEvent>,
    tray: Option<TrayIcon>,
    /// 사용량·계정·로그인·업데이트·설정 화면 처리 (Windows와 같은 usagi-core 코드)
    ctl: Controller,
    title_sig: String,
    menu_sig: String,
    settings: Option<SettingsWindow>,
    /// 끝내야 함 (종료 명령, 업데이트 적용)
    exit: bool,
}

/// 진단 기록 (설정 폴더의 usagi-mac.log, 최근 것만)
pub fn log(msg: &str) {
    use std::io::Write;
    let dir = config::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("usagi-mac.log");
    if std::fs::metadata(&path).map(|m| m.len() > 256 * 1024).unwrap_or(false) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{}] {msg}", time::now() as i64);
    }
}

/// Finder에서 실행한 앱은 터미널의 PATH를 모름 → 로그인 셸에서 PATH를 받아 옴 (claude, codex, node 찾기)
fn fix_path() {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    if let Ok(out) = Command::new(&shell).args(["-lc", "printf %s \"$PATH\""]).output() {
        let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !p.is_empty() {
            std::env::set_var("PATH", p);
            return;
        }
    }
    let cur = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("/opt/homebrew/bin:/usr/local/bin:{cur}"));
}

pub fn run() {
    log(&format!("시작 v{}", usagi_core::VERSION));
    fix_path();
    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    event_loop.set_activation_policy(ActivationPolicy::Accessory); // Dock 아이콘 없이 메뉴 막대만

    let proxy = event_loop.create_proxy();
    {
        let proxy = proxy.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let _ = proxy.send_event(UserEvent::Menu(e));
        }));
    }
    let latest: Arc<Mutex<Option<Snapshot>>> = Arc::new(Mutex::new(None));
    let engine = {
        let latest = latest.clone();
        let proxy = proxy.clone();
        Engine::start(move |snap| {
            if let Ok(mut g) = latest.lock() {
                *g = Some(snap);
            }
            let _ = proxy.send_event(UserEvent::Snapshot);
        })
    };
    let managed_update = crate::updater::installed_by_brew().then_some(crate::updater::BREW_HINT);
    let mut app = App {
        proxy,
        tray: None,
        ctl: Controller::new(engine, Platform { name: "mac", repo: crate::updater::REPO, managed_update }),
        title_sig: String::new(),
        menu_sig: String::new(),
        settings: None,
        exit: false,
    };

    event_loop.run(move |event, target, control_flow| {
        match event {
            Event::NewEvents(StartCause::Init) => {
                app.create_tray();
                let fx = app.ctl.startup();
                app.run(fx, target);
            }
            Event::NewEvents(StartCause::ResumeTimeReached { .. }) => {
                // 로그인 진행·재확인, 업데이트 확인 시각은 컨트롤러가 스스로 셈
                let fx = app.ctl.tick();
                if !fx.is_empty() {
                    app.run(fx, target);
                }
            }
            Event::UserEvent(UserEvent::Snapshot) => {
                if let Some(s) = latest.lock().ok().and_then(|mut g| g.take()) {
                    let fx = app.ctl.apply_snapshot(s);
                    app.run(fx, target);
                }
            }
            Event::UserEvent(UserEvent::Menu(e)) => app.on_menu(e.id().0.as_str(), target),
            // 실행 중에 Finder·Launchpad에서 다시 열면 설정 창 (Windows에서 한 번 더 실행할 때와 같음)
            Event::Reopen { .. } => app.open_settings(target),
            Event::UserEvent(UserEvent::Ipc(raw)) => {
                // 진단: 화면 명령 이름과 결과 오류만 (값·이메일은 남기지 않음)
                let msg: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
                let cmd = msg.get("cmd").and_then(|c| c.as_str()).unwrap_or("?").to_string();
                if cmd == "ready" {
                    log("설정 화면 준비됨");
                } else {
                    let target_id = msg.get("id").or_else(|| msg.get("provider")).and_then(|v| v.as_str()).unwrap_or("");
                    log(&format!("화면 명령: {cmd} {target_id}"));
                }
                let fx = app.ctl.handle_ipc(&raw);
                app.run(fx, target);
                if let Some(e) = app.ctl.error() {
                    log(&format!("화면 오류: {e}"));
                }
            }
            Event::UserEvent(UserEvent::Update(ev)) => {
                let fx = match ev {
                    UpdateEvent::Checked(r, manual) => app.ctl.update_checked(r, manual),
                    UpdateEvent::Downloaded(rel, r, install) => app.ctl.update_downloaded(rel, r, install),
                };
                app.run(fx, target);
            }
            Event::WindowEvent { window_id, event: WindowEvent::CloseRequested, .. } => {
                if app.settings.as_ref().is_some_and(|s| s.id() == window_id) {
                    app.run(vec![Effect::CloseSettings], target);
                }
            }
            _ => {}
        }
        *control_flow = ControlFlow::WaitUntil(Instant::now() + if app.ctl.busy() { FRAME } else { IDLE_FRAME });
        if app.exit {
            app.ctl.engine.stop();
            app.settings = None;
            app.tray = None;
            *control_flow = ControlFlow::Exit;
        }
    });
}

impl App {
    fn create_tray(&mut self) {
        let menu = Menu::new();
        let _ = menu.append(&MenuItem::with_id("settings", "설정 열기…", true, None));
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&MenuItem::with_id("quit", "usAgI 종료", true, None));
        // 토끼 아이콘 없이 글자만 (공급자 로고는 글자 안에)
        match TrayIconBuilder::new().with_menu(Box::new(menu)).with_title("usAgI").with_tooltip("usAgI").build() {
            Ok(t) => {
                log("메뉴 막대 항목 만듦");
                self.tray = Some(t);
            }
            Err(e) => {
                log(&format!("메뉴 막대 항목 실패: {e}"));
                notify("usAgI", &format!("메뉴 막대 표시 실패: {e}"));
            }
        }
    }

    /// 공통 컨트롤러(usagi-core)가 돌려준 일을 실행. 설정 화면은 마지막에 한 번만 다시 보냄.
    fn run(&mut self, effects: Vec<Effect>, target: &EventLoopWindowTarget<UserEvent>) {
        let mut queue: std::collections::VecDeque<Effect> = effects.into();
        let mut push = false;
        while let Some(e) = queue.pop_front() {
            match e {
                Effect::Push => push = true,
                Effect::Redraw => self.refresh_ui(),
                // 위젯 위치·모니터·중요 알림은 Windows 전용
                Effect::PlatformPref(_) => {}
                Effect::Open(target) => open(&target),
                Effect::OpenText(path) => {
                    let _ = Command::new("/usr/bin/open").arg("-t").arg(path).spawn();
                }
                Effect::OpenNotifySettings => open("x-apple.systempreferences:com.apple.Notifications-Settings.extension"),
                Effect::Notify(title, body) => notify(&title, &body),
                Effect::SetAutostart(on) => {
                    if let Err(e) = set_autostart(on) {
                        self.ctl.set_error(e);
                    }
                }
                Effect::CloseSettings => {
                    self.settings = None;
                    target.set_activation_policy_at_runtime(ActivationPolicy::Accessory);
                    queue.extend(self.ctl.settings_closed());
                }
                Effect::Quit => {
                    self.exit = true;
                    return;
                }
                Effect::CheckUpdate { manual } => {
                    let proxy = self.proxy.clone();
                    std::thread::spawn(move || {
                        let _ = proxy.send_event(UserEvent::Update(UpdateEvent::Checked(crate::updater::check(), manual)));
                    });
                }
                Effect::DownloadUpdate { release, install } => {
                    let proxy = self.proxy.clone();
                    std::thread::spawn(move || {
                        let r = crate::updater::download(&release);
                        let _ = proxy.send_event(UserEvent::Update(UpdateEvent::Downloaded(release, r, install)));
                    });
                }
                Effect::ApplyUpdate(staged) => match crate::updater::apply(&staged) {
                    Ok(()) => {
                        self.exit = true;
                        return;
                    }
                    Err(e) => queue.extend(self.ctl.update_apply_failed(e)),
                },
            }
        }
        if push {
            self.push_settings();
        }
    }

    // -------------------------------------------------------------- 메뉴 막대·메뉴

    fn time_mode(&self) -> bool {
        self.ctl.time_mode()
    }

    /// 메뉴 막대 글자 값 (짧게)
    fn short_value(&self, acc: &str, m: &Meter, now: f64) -> String {
        if !m.exact {
            return "–".into();
        }
        if self.time_mode() {
            return m.resets_at.filter(|&r| r > now).map(|r| texts::fmt_hm_short(r - now)).unwrap_or_else(|| "–".into());
        }
        let left = ui::meter_left(self.ctl.state(), &alert_key(acc, &m.label));
        texts::fmt_pct(if left { (1.0 - m.ratio).max(0.0) } else { m.ratio })
    }

    fn refresh_ui(&mut self) {
        let Some(snap) = &self.ctl.snap else { return };
        let now = time::now();
        let state = self.ctl.state();
        let accounts = accounts::load(state);

        // ---- 메뉴 막대: [로고] 5h 53%  7d 64%  │  [로고] 7d 92%
        let mut parts = Vec::new();
        for acc in accounts.iter().filter(|a| a.show) {
            let Some(v) = view(snap, acc) else { continue };
            let items: Vec<(String, String)> = v
                .meters
                .iter()
                .filter(|m| ui::meter_in_widget(state, &alert_key(&acc.id, &m.label)))
                .map(|m| (crate::title::short_label(&m.label), self.short_value(&acc.id, m, now)))
                .collect();
            if !items.is_empty() {
                parts.push(crate::title::Part { provider: v.id, items });
            }
        }
        let dark = self.tray.as_ref().is_some_and(crate::title::is_dark);
        let sig = parts.iter().map(|p| format!("{:?}{:?}", p.provider, p.items)).collect::<Vec<_>>().join("|") + if dark { "D" } else { "L" };
        if sig != self.title_sig {
            if let Some(t) = &self.tray {
                crate::title::apply(t, &parts);
            }
            // 진단: 메뉴 막대에 보인 글자 (바뀔 때만)
            let text = parts
                .iter()
                .map(|p| format!("[{}] {}", p.provider.name(), p.items.iter().map(|(l, v)| format!("{l} {v}")).collect::<Vec<_>>().join("  ")))
                .collect::<Vec<_>>()
                .join(" │ ");
            log(&format!("메뉴 막대: {}", if text.is_empty() { "(사용량 없음)" } else { &text }));
            self.title_sig = sig;
        }

        // ---- 메뉴: 계정별 자세히
        let mut lines: Vec<(String, String, bool)> = Vec::new(); // (id, 글자, 누를 수 있음)
        for acc in &accounts {
            let login = snap.logins.get(&acc.id);
            let v = view(snap, acc);
            let email = v.and_then(|v| v.email.clone()).or_else(|| login.and_then(|l| l.email.clone()));
            let name = if !acc.label.is_empty() {
                acc.label.clone()
            } else {
                email.unwrap_or_else(|| if acc.is_claude() { "Claude".into() } else { "ChatGPT".into() })
            };
            let plan = if acc.is_claude() {
                login.and_then(|l| l.plan.clone()).map(|p| p.trim_start_matches("Claude ").trim_end_matches(" account").to_string())
            } else {
                v.and_then(|v| v.plan.as_deref()).map(codex::plan_label).filter(|p| !p.is_empty())
            };
            let brand = if acc.is_claude() { "Claude" } else { "ChatGPT" };
            let head = match plan {
                Some(p) => format!("{brand} · {name} · {p}"),
                None => format!("{brand} · {name}"),
            };
            lines.push((format!("head:{}", acc.id), head, false));
            if login.and_then(|l| l.logged_in) == Some(false) {
                lines.push((format!("login:{}", acc.id), "    로그인 필요 — 눌러서 로그인".into(), true));
                continue;
            }
            match v {
                Some(v) if !v.meters.is_empty() => {
                    for m in &v.meters {
                        let left = ui::meter_left(state, &alert_key(&acc.id, &m.label));
                        let val = if m.exact {
                            let pct = ((if left { 1.0 - m.ratio } else { m.ratio }) * 100.0).round().max(0.0);
                            format!("{pct:.0}% {}", if left { "남음" } else { "사용" })
                        } else {
                            "–".into()
                        };
                        let reset = m
                            .resets_at
                            .filter(|&r| r > now && m.exact)
                            .map(|r| format!(" · {} 뒤 리셋 ({})", texts::fmt_duration(r - now), texts::fmt_clock(r, now)))
                            .unwrap_or_default();
                        lines.push((format!("m:{}:{}", acc.id, m.label), format!("    {}  {val}{reset}", m.label), false));
                    }
                }
                _ => lines.push((format!("wait:{}", acc.id), "    사용량을 받는 중…".into(), false)),
            }
        }
        let mode = if self.time_mode() { "퍼센트로 보기" } else { "남은 시간으로 보기" };
        let sig = format!("{lines:?}{mode}");
        if sig == self.menu_sig {
            return;
        }
        self.menu_sig = sig;
        let menu = Menu::new();
        let _ = menu.append(&MenuItem::with_id("settings", "설정 열기…", true, None));
        let _ = menu.append(&PredefinedMenuItem::separator());
        for (i, (id, text, enabled)) in lines.iter().enumerate() {
            if id.starts_with("head:") && i > 0 {
                let _ = menu.append(&PredefinedMenuItem::separator());
            }
            let _ = menu.append(&MenuItem::with_id(id.as_str(), text, *enabled, None));
        }
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&MenuItem::with_id("refresh", "지금 새로고침", true, None));
        let _ = menu.append(&MenuItem::with_id("mode", mode, true, None));
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&MenuItem::with_id("quit", "usAgI 종료", true, None));
        if let Some(t) = &self.tray {
            t.set_menu(Some(Box::new(menu)));
        }
    }

    /// 메뉴 누름
    fn on_menu(&mut self, id: &str, target: &EventLoopWindowTarget<UserEvent>) {
        match id {
            "quit" => self.exit = true,
            "settings" => self.open_settings(target),
            "refresh" => self.ctl.refresh_all(),
            "mode" => {
                let fx = self.ctl.toggle_mode();
                self.run(fx, target);
            }
            other => {
                if let Some(acc_id) = other.strip_prefix("login:") {
                    // 설정 창에서 공식 로그인 진행 상황을 보여 줌
                    self.open_settings(target);
                    self.ctl.login_account(acc_id);
                    self.push_settings();
                }
            }
        }
    }

    // -------------------------------------------------------------- 설정 창

    fn open_settings(&mut self, target: &EventLoopWindowTarget<UserEvent>) {
        // 창을 앞으로 가져오려면 잠깐 일반 앱이 됨 (Dock 아이콘), 닫으면 다시 메뉴 막대 앱으로
        target.set_activation_policy_at_runtime(ActivationPolicy::Regular);
        target.show_application();
        if let Some(s) = &self.settings {
            s.focus();
            return;
        }
        match SettingsWindow::open(target, self.proxy.clone()) {
            Ok(s) => {
                self.settings = Some(s);
                self.ctl.settings_opened();
            }
            Err(e) => {
                log(&e);
                notify("usAgI", &e);
                target.set_activation_policy_at_runtime(ActivationPolicy::Accessory);
            }
        }
    }

    /// macOS에서 읽는 값: 로그인할 때 실행 (위젯 모니터·중요 알림은 없음)
    fn host_info(&self) -> HostInfo {
        HostInfo { autostart: autostart_enabled(), ..HostInfo::default() }
    }

    fn push_settings(&self) {
        if let Some(s) = &self.settings {
            s.push(&self.ctl.settings_state(&self.host_info()).to_string());
        }
    }
}

fn view<'a>(snap: &'a Snapshot, acc: &Account) -> Option<&'a ProviderView> {
    snap.providers.iter().find(|p| p.account_id == acc.id)
}

fn open(target: &str) {
    let _ = Command::new("/usr/bin/open").arg(target).spawn();
}

fn esc_applescript(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// macOS 알림 (알림 센터)
pub fn notify(title: &str, msg: &str) {
    let script = format!("display notification \"{}\" with title \"{}\"", esc_applescript(msg), esc_applescript(title));
    let _ = Command::new("/usr/bin/osascript").args(["-e", &script]).spawn();
}

// ------------------------------------------------------------------ 로그인할 때 실행 (LaunchAgent)

const AGENT_LABEL: &str = "dev.usagi.usagi";

fn agent_path() -> Option<std::path::PathBuf> {
    usagi_core::logs::home_dir().map(|h| h.join("Library/LaunchAgents").join(format!("{AGENT_LABEL}.plist")))
}

fn autostart_enabled() -> bool {
    agent_path().is_some_and(|p| p.exists())
}

/// .app 묶음 경로 (…/usAgI.app). 묶음 밖에서 실행 중이면 exe 그대로.
fn app_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bundle = exe.parent()?.parent()?.parent()?;
    Some(if bundle.extension().is_some_and(|e| e == "app") { bundle.to_path_buf() } else { exe })
}

fn set_autostart(on: bool) -> Result<(), String> {
    let path = agent_path().ok_or("홈 폴더 찾기 실패")?;
    if !on {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    let app = app_path().ok_or("앱 위치 찾기 실패")?;
    let target = app.to_string_lossy().replace('&', "&amp;").replace('<', "&lt;");
    let args = if app.extension().is_some_and(|e| e == "app") {
        format!("<string>/usr/bin/open</string><string>-a</string><string>{target}</string>")
    } else {
        format!("<string>{target}</string>")
    };
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{AGENT_LABEL}</string>
  <key>ProgramArguments</key><array>{args}</array>
  <key>RunAtLoad</key><true/>
</dict>
</plist>
"#
    );
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    std::fs::write(&path, plist).map_err(|e| format!("로그인할 때 실행 켜기 실패: {e}"))
}

/// 앱 아이콘 .iconset (빌드 스크립트가 iconutil로 .icns를 만듦)
pub fn write_iconset(dir: &std::path::Path) {
    let _ = std::fs::create_dir_all(dir);
    for (n, name) in [
        (16, "icon_16x16.png"),
        (32, "icon_16x16@2x.png"),
        (32, "icon_32x32.png"),
        (64, "icon_32x32@2x.png"),
        (128, "icon_128x128.png"),
        (256, "icon_128x128@2x.png"),
        (256, "icon_256x256.png"),
        (512, "icon_256x256@2x.png"),
        (512, "icon_512x512.png"),
        (1024, "icon_512x512@2x.png"),
    ] {
        if let Some(png) = usagi_core::rabbit::app_icon(n).and_then(|pm| pm.encode_png().ok()) {
            let _ = std::fs::write(dir.join(name), png);
        }
    }
}
