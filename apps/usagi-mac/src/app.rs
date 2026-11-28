//! macOS 메뉴 막대 앱.
//!
//! - 메뉴 막대: [로고] 5h 53%  7d 64%  │  [로고] 7d 92% (토끼 없이, 공급자 로고만)
//! - 누르면: 설정 열기, 새로고침, 보기 바꾸기, 종료
//! - 설정 창: Windows와 같은 Slint 화면, 화면 상태와 명령 처리도 Windows와 같은
//!   usagi-core 컨트롤러. 이 파일은 메뉴 막대·창·알림·로그인할 때 실행·업데이트 적용만 맡음.

use usagi_core::settings_window::SettingsWindow;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};
use usagi_core::accounts::{self, Account};
use usagi_core::config;
use usagi_core::controller::{Controller, Effect, HostInfo, Platform};
use usagi_core::engine::{alert_key, Engine, Meter, ProviderView, Snapshot};
use usagi_core::update::Release;
use usagi_core::{b64, texts, time, ui};

pub enum UserEvent {
    Snapshot,
    Menu(MenuEvent),
    SettingsInput,
    Update(UpdateEvent),
    /// 알림을 누름
    OpenSettings,
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
    /// 설정 창에 마지막으로 보낸 메뉴 막대 미리 보기 (같으면 다시 그리지 않음)
    preview_sent: std::cell::Cell<u64>,
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

/// Finder에서 실행한 앱은 터미널의 PATH를 모름 → 로그인 셸에서 PATH를 받아 옴 (claude, codex, node 찾기).
/// rc 파일이 다른 글자를 출력해도 표시 사이의 값만 읽고, 상대 경로 항목(빈 항목·`.` 등)은 뺌. 셸이 멈추면 5초 뒤 포기
fn fix_path() {
    let shell = std::env::var_os("SHELL")
        .map(std::path::PathBuf::from)
        .filter(|shell| shell.is_absolute() && shell.is_file())
        .unwrap_or_else(|| "/bin/zsh".into());
    let mut command = Command::new(&shell);
    command.args(["-lc", usagi_core::login::LOGIN_PATH_SCRIPT]);
    let found = usagi_core::login::run_bounded_command(command, "", std::time::Duration::from_secs(5))
        .as_deref()
        .and_then(usagi_core::login::parse_login_path);
    if let Some(path) = found {
        std::env::set_var("PATH", path);
        return;
    }
    let cur = std::env::var("PATH").unwrap_or_default();
    let fallback = format!("/opt/homebrew/bin:/usr/local/bin:{cur}");
    let path = usagi_core::login::absolute_paths(fallback.as_ref()).unwrap_or_else(|| "/usr/bin:/bin:/usr/sbin:/sbin".into());
    std::env::set_var("PATH", path);
}

pub fn run() {
    let dir = config::config_dir();
    let lock = std::fs::create_dir_all(&dir).and_then(|()| {
        std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false)
            .open(dir.join("instance.lock"))
    });
    let Ok(instance) = lock else {
        log("Could not open usAgI instance lock");
        return;
    };
    if instance.try_lock().is_err() { return; }
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
        preview_sent: std::cell::Cell::new(0),
        exit: false,
    };

    let mut scheduled_tick: Option<Instant> = None;
    event_loop.run(move |event, _, control_flow| {
        let _instance = &instance;
        match event {
            Event::NewEvents(StartCause::Init) => {
                let proxy = app.proxy.clone();
                crate::notify::init(move || {
                    let _ = proxy.send_event(UserEvent::OpenSettings);
                });
                app.create_tray();
                let fx = app.ctl.startup();
                app.run(fx);
            }
            Event::NewEvents(_) if scheduled_tick.is_some_and(|at| Instant::now() >= at) => {
                scheduled_tick = None;
                let fx = app.ctl.tick();
                if !fx.is_empty() {
                    app.run(fx);
                }
            }
            Event::UserEvent(UserEvent::Snapshot) => {
                if let Some(s) = latest.lock().ok().and_then(|mut g| g.take()) {
                    let fx = app.ctl.apply_snapshot(s);
                    app.run(fx);
                }
            }
            Event::UserEvent(UserEvent::Menu(e)) => app.on_menu(e.id().0.as_str()),
            // 실행 중에 Finder·Launchpad에서 다시 열면 설정 창 (Windows에서 한 번 더 실행할 때와 같음)
            Event::Reopen { .. } | Event::UserEvent(UserEvent::OpenSettings) => app.open_settings(),
            Event::UserEvent(UserEvent::SettingsInput) => {
                while let Some(raw) = app.settings.as_ref().and_then(SettingsWindow::next_command) {
                    let fx = app.ctl.handle_ipc(&raw);
                    app.run(fx);
                }
            }
            Event::UserEvent(UserEvent::Update(ev)) => {
                let fx = match ev {
                    UpdateEvent::Checked(r, manual) => app.ctl.update_checked(r, manual),
                    UpdateEvent::Downloaded(rel, r, install) => app.ctl.update_downloaded(rel, r, install),
                };
                app.run(fx);
            }
            _ => {}
        }
        if app.exit {
            app.ctl.engine.stop();
            app.settings = None;
            app.tray = None;
            *control_flow = ControlFlow::Exit;
        } else {
            scheduled_tick = app.ctl.next_tick_delay(time::now()).map(|delay| {
                let next = Instant::now() + delay;
                scheduled_tick.map_or(next, |previous| previous.min(next))
            });
            *control_flow = scheduled_tick.map_or(ControlFlow::Wait, ControlFlow::WaitUntil);
        }
    });
}

impl App {
    fn create_tray(&mut self) {
        // 토끼 아이콘 없이 글자만 (공급자 로고는 글자 안에)
        match TrayIconBuilder::new().with_menu(Box::new(base_menu())).with_title("usAgI").with_tooltip("usAgI").build() {
            Ok(t) => {
                log("메뉴 막대 항목 만듦");
                self.tray = Some(t);
            }
            Err(e) => {
                log(&format!("메뉴 막대 항목 실패: {e}"));
                notify("usAgI", &usagi_core::tr!(format!("메뉴 막대 표시 실패: {e}"), format!("Couldn't show the menu bar item: {e}")));
            }
        }
    }

    /// 공통 컨트롤러(usagi-core)가 돌려준 일을 실행. 설정 화면은 마지막에 한 번만 다시 보냄.
    fn run(&mut self, effects: Vec<Effect>) {
        let mut queue: std::collections::VecDeque<Effect> = effects.into();
        let mut push = false;
        while let Some(e) = queue.pop_front() {
            match e {
                Effect::Push => push = true,
                Effect::Redraw => self.refresh_ui(),
                // 언어가 바뀜: 메뉴를 새 언어로 다시 만듦 (뒤따르는 Redraw가 사용량 메뉴를 다시 채움)
                Effect::PlatformPref(key) if key == "locale" => {
                    self.menu_sig.clear();
                    if self.ctl.snap.is_none() {
                        if let Some(t) = &self.tray {
                            t.set_menu(Some(Box::new(base_menu())));
                        }
                    }
                }
                // 위젯 위치·모니터·중요 알림은 Windows 전용
                Effect::PlatformPref(_) | Effect::WidgetBarLength => {}
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
                // macOS 앱은 Finder·설치 스크립트로 옮김 (화면에도 옮기기 단추를 보이지 않음)
                Effect::InstallCopy => {}
                Effect::CloseSettings => {
                    self.settings = None;
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
        texts::fmt_usage(m.ratio, left)
    }

    /// 메뉴 막대 칸: [로고] 5h ▬▬ 53%  7d ▬▬ 64%  │  [로고] 7d ▬▬ 92% (사용량을 받기 전엔 빈 목록)
    fn title_parts(&self, now: f64) -> Vec<crate::title::Part> {
        let Some(snap) = &self.ctl.snap else { return Vec::new() };
        let state = self.ctl.state();
        let bars = state.pref_bool("bars", true);
        let mut parts = Vec::new();
        for acc in accounts::load(state).iter().filter(|a| a.show) {
            let Some(v) = view(snap, acc) else { continue };
            // 로그아웃: 예전 값 대신 "로그인 필요"
            if snap.logged_out(&acc.id) {
                parts.push(crate::title::Part { provider: v.id, items: Vec::new(), note: Some(texts::login_needed().into()) });
                continue;
            }
            let items: Vec<crate::title::Item> = v
                .meters
                .iter()
                .filter(|m| ui::meter_in_widget(state, &alert_key(&acc.id, &m.label)))
                .map(|m| {
                    let left = ui::meter_left(state, &alert_key(&acc.id, &m.label));
                    // 남은 양 기준이면 줄어드는 막대 (색은 사용량 기준 그대로, Windows 위젯과 같음)
                    let fill = if !m.exact { 0.0 } else if left { (1.0 - m.ratio).max(0.0) } else { m.ratio };
                    crate::title::Item {
                        label: crate::title::short_label(&m.label),
                        bar: bars.then_some(crate::title::Bar { fill, level: texts::level(m.ratio) }),
                        value: self.short_value(&acc.id, m, now),
                    }
                })
                .collect();
            if !items.is_empty() {
                parts.push(crate::title::Part { provider: v.id, items, note: None });
            }
        }
        parts
    }

    fn refresh_ui(&mut self) {
        if self.ctl.snap.is_none() {
            return;
        }
        let parts = self.title_parts(time::now());
        let dark = self.tray.as_ref().is_some_and(crate::title::is_dark);
        let sig = parts.iter().map(|p| format!("{:?}{:?}{:?}", p.provider, p.items, p.note)).collect::<Vec<_>>().join("|") + if dark { "D" } else { "L" };
        if sig != self.title_sig {
            if let Some(t) = &self.tray {
                crate::title::apply(t, &parts, dark);
            }
            // 진단: 메뉴 막대에 보인 글자 (바뀔 때만)
            let text = parts
                .iter()
                .map(|p| {
                    let items = p.items.iter().map(|i| format!("{}{} {}", i.label, if i.bar.is_some() { " ▬" } else { "" }, i.value));
                    format!("[{}] {}{}", p.provider.name(), items.collect::<Vec<_>>().join("  "), p.note.as_deref().unwrap_or(""))
                })
                .collect::<Vec<_>>()
                .join(" │ ");
            log(&format!("메뉴 막대: {}", if text.is_empty() { "(사용량 없음)" } else { &text }));
            self.title_sig = sig;
        }

        // ---- 메뉴: 명령만 (사용량은 메뉴 막대와 설정 창에서 봄)
        let mode = if self.time_mode() {
            usagi_core::tr!("퍼센트로 보기", "Show percent")
        } else {
            usagi_core::tr!("남은 시간으로 보기", "Show remaining time")
        };
        // 언어도 넣어 둠: 바뀌면 항목 글자를 새 언어로 다시 만듦
        let sig = format!("{mode}{}", usagi_core::i18n::lang().code());
        if sig == self.menu_sig {
            return;
        }
        self.menu_sig = sig;
        let menu = Menu::new();
        let _ = menu.append(&MenuItem::with_id("settings", usagi_core::tr!("설정 열기…", "Open settings…"), true, None));
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&MenuItem::with_id("refresh", usagi_core::tr!("지금 새로고침", "Refresh now"), true, None));
        let _ = menu.append(&MenuItem::with_id("mode", mode, true, None));
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&MenuItem::with_id("quit", usagi_core::tr!("usAgI 종료", "Quit usAgI"), true, None));
        if let Some(t) = &self.tray {
            t.set_menu(Some(Box::new(menu)));
        }
    }

    /// 메뉴 누름
    fn on_menu(&mut self, id: &str) {
        match id {
            "quit" => self.exit = true,
            "settings" => self.open_settings(),
            "refresh" => self.ctl.refresh_all(),
            "mode" => {
                let fx = self.ctl.toggle_mode();
                self.run(fx);
            }
            _ => {}
        }
    }

    // -------------------------------------------------------------- 설정 창

    fn open_settings(&mut self) {
        if let Some(s) = &self.settings {
            s.focus();
            return;
        }
        let proxy = self.proxy.clone();
        match crate::settings_program().and_then(|program| SettingsWindow::open(program, move || {
            let _ = proxy.send_event(UserEvent::SettingsInput);
        })) {
            Ok(s) => {
                self.settings = Some(s);
                self.preview_sent.set(0);
                self.ctl.settings_opened();
            }
            Err(e) => {
                log(&e.to_string());
                notify("usAgI", &e.to_string());
            }
        }
    }

    /// macOS에서 읽는 값: 로그인할 때 실행, 메뉴 막대 미리 보기 (위젯 모니터·중요 알림은 없음)
    fn host_info(&self) -> HostInfo {
        HostInfo { autostart: autostart_enabled(), widget_preview: self.menubar_preview(), ..HostInfo::default() }
    }

    /// 설정 창 미리 보기: 지금 메뉴 막대 글자를 그림으로 (바뀌지 않았으면 그대로 두라는 표시만)
    fn menubar_preview(&self) -> Option<serde_json::Value> {
        use std::hash::{DefaultHasher, Hash, Hasher};
        if !self.ctl.wants_widget_preview() {
            self.preview_sent.set(0);
            return None;
        }
        let parts = self.title_parts(time::now());
        let dark = self.tray.as_ref().is_some_and(crate::title::is_dark);
        let mut h = DefaultHasher::new();
        (parts.iter().map(|p| format!("{:?}{:?}{:?}", p.provider, p.items, p.note)).collect::<Vec<_>>(), dark, usagi_core::i18n::lang().code()).hash(&mut h);
        let key = h.finish() | 1;
        if self.preview_sent.get() == key {
            return Some(serde_json::json!({ "keep": true }));
        }
        let p = crate::title::preview(&parts, dark)?;
        self.preview_sent.set(key);
        Some(serde_json::json!({ "w": p.w, "h": p.h, "scale": p.scale, "light": !dark, "rgba": b64::encode(&p.rgba) }))
    }

    fn push_settings(&self) {
        if let Some(s) = &self.settings {
            s.push(self.ctl.settings_state(&self.host_info()).to_string());
        }
    }
}

/// 사용량을 받기 전의 메뉴 (설정 열기·종료만). 언어를 바꾸면 이걸로 다시 만듦
fn base_menu() -> Menu {
    let menu = Menu::new();
    let _ = menu.append(&MenuItem::with_id("settings", usagi_core::tr!("설정 열기…", "Open settings…"), true, None));
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id("quit", usagi_core::tr!("usAgI 종료", "Quit usAgI"), true, None));
    menu
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

/// macOS 알림 (알림 센터). 앱 자체 알림을 쓸 수 없을 때만 osascript
pub fn notify(title: &str, msg: &str) {
    use std::io::Write;
    use std::process::Stdio;
    if crate::notify::send(title, msg) {
        return;
    }
    let script = format!("display notification \"{}\" with title \"{}\"", esc_applescript(msg), esc_applescript(title));
    let child = Command::new("/usr/bin/osascript").stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
    if let Ok(mut child) = child {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(script.as_bytes());
        }
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// 디자인 확인용: 예시 값으로 메뉴 막대 미리 보기를 PNG로 (인자: 저장 경로, light, nobars, spent, time)
#[cfg(debug_assertions)]
pub fn write_menubar_preview(args: &[String]) {
    use crate::title::{Bar, Item, Part};
    use usagi_core::engine::ProviderId;
    let has = |f: &str| args.iter().any(|a| a == f);
    let path = args.first().cloned().unwrap_or_else(|| "menubar.png".into());
    let (bars, time_mode) = (!has("nobars"), has("time"));
    let item = |label: &str, ratio: f64, left: f64| Item {
        label: label.into(),
        bar: bars.then_some(Bar { fill: ratio, level: texts::level(ratio) }),
        value: if time_mode { texts::fmt_hm_short(left) } else { texts::fmt_usage(ratio, false) },
    };
    let first = if has("spent") { 1.0 } else { 0.62 };
    let claude = if has("loggedout") {
        Part { provider: ProviderId::Claude, items: vec![], note: Some(texts::login_needed().into()) }
    } else {
        Part { provider: ProviderId::Claude, items: vec![item("5h", first, 2.0 * 3600.0 + 14.0 * 60.0), item("7d", 0.38, 4.0 * 86400.0 + 17.0 * 3600.0)], note: None }
    };
    let parts = vec![claude, Part { provider: ProviderId::Codex, items: vec![item("5h", 0.47, 3.0 * 3600.0), item("7d", 0.12, 5.0 * 86400.0)], note: None }];
    let Some(p) = crate::title::preview(&parts, !has("light")) else {
        eprintln!("미리 보기 실패");
        return;
    };
    let mut px = p.rgba.clone();
    for c in px.chunks_exact_mut(4) {
        c[3] = 255;
    }
    match tiny_skia::Pixmap::from_vec(px, tiny_skia::IntSize::from_wh(p.w, p.h).unwrap()).map(|pm| pm.save_png(&path)) {
        Some(Ok(())) => println!("{path} {}x{}", p.w, p.h),
        _ => eprintln!("저장 실패"),
    }
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
    let path = agent_path().ok_or(usagi_core::tr!("홈 폴더 찾기 실패", "Couldn't find the home folder"))?;
    if !on {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(usagi_core::tr!(
                format!("시작 시 실행 해제 실패 ({error})"),
                format!("Could not turn off start at login ({error})")
            )),
        };
    }
    // 내려받기·임시 폴더(격리된 앱이 옮겨져 실행되는 App Translocation 포함)의 경로는 등록하지 않음
    if let Some(kind) = usagi_core::location::current() {
        return Err(kind.blocked(("로그인 시 실행", "start at login")));
    }
    let app = app_path().ok_or(usagi_core::tr!("앱 위치 찾기 실패", "Couldn't find the app location"))?;
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
    std::fs::write(&path, plist).map_err(|e| usagi_core::tr!(format!("로그인 시 실행 켜기 실패: {e}"), format!("Couldn't turn on open at login: {e}")))
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
        if let Some(png) = mac_app_icon(n).and_then(|pm| pm.encode_png().ok()) {
            let _ = std::fs::write(dir.join(name), png);
        }
    }
}

fn mac_app_icon(size: u32) -> Option<tiny_skia::Pixmap> {
    use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, Transform};

    let mut icon = Pixmap::new(size, size)?;
    // Leave a transparent margin around the rounded macOS icon tile.
    let mut tile = PathBuilder::new();
    tile.move_to(29.0, 10.0);
    tile.line_to(71.0, 10.0);
    tile.quad_to(90.0, 10.0, 90.0, 29.0);
    tile.line_to(90.0, 71.0);
    tile.quad_to(90.0, 90.0, 71.0, 90.0);
    tile.line_to(29.0, 90.0);
    tile.quad_to(10.0, 90.0, 10.0, 71.0);
    tile.line_to(10.0, 29.0);
    tile.quad_to(10.0, 10.0, 29.0, 10.0);
    tile.close();

    let mut black = Paint::default();
    black.set_color_rgba8(0, 0, 0, 255);
    black.anti_alias = true;
    let scale = size as f32 / 100.0;
    icon.fill_path(&tile.finish()?, &black, FillRule::Winding, Transform::from_scale(scale, scale), None);

    let rabbit_size = (size as f32 * 0.70).round() as u32;
    let rabbit = usagi_core::rabbit::app_icon(rabbit_size)?;
    let offset = ((size - rabbit_size) / 2) as i32;
    icon.draw_pixmap(offset, offset, rabbit.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
    Some(icon)
}
