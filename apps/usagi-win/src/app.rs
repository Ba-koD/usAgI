//! usAgI Windows 앱: 숨은 호스트 창(타이머·트레이 콜백) + 작업 표시줄 위젯(모니터별) + 설정 창.
//!
//! 모든 창이 하나의 WndProc을 쓰고, 상태는 GUI 스레드의 `APP`에 있습니다.
//! 메뉴처럼 메시지 루프를 도는 호출 중에는 APP이 빌려져 있으므로
//! 그 사이 들어온 메시지는 기본 처리로 넘어갑니다 (타이머/스냅숏은 곧 다시 옴).

use crate::settings::{self, SettingsWindow, WM_IPC};
use crate::tray::{Tray, WM_TRAY};
use crate::widget::{Content, Row, Section, Side, Widget};
use crate::win::{self, wide, Rgb};
use serde_json::json;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;
use usagi_core::animator::{Animator, Pose};
use usagi_core::config;
use usagi_core::controller::{Controller, Effect, HostInfo, Platform};
use usagi_core::engine::{Engine, Snapshot};
use usagi_core::texts::{self, ValueMode};
use usagi_core::update::Release;
use usagi_core::{rabbit, time};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Ole::OleInitialize;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

const HOST_CLASS: &str = "usAgIHost";
const WIDGET_CLASS: &str = "usAgIWidget";

const WM_SNAPSHOT: u32 = WM_APP + 10;
const WM_FOREGROUND: u32 = WM_APP + 12;
pub const WM_SHOW_SETTINGS: u32 = WM_APP + 13;
const WM_UPDATE: u32 = WM_APP + 14;

const TIMER_ANIM: usize = 10;
const TIMER_PLACE: usize = 11;
const TIMER_RECHECK: usize = 12;
const TIMER_RETRY_ADD: usize = 13;
const FAST_FRAME_MS: u32 = 15;


static HOST: AtomicIsize = AtomicIsize::new(0);
static LATEST: Mutex<Option<Snapshot>> = Mutex::new(None);
/// 업데이트 백그라운드 작업 결과 (WM_UPDATE로 알림)
static UPDATE_EVENTS: Mutex<Vec<UpdateEvent>> = Mutex::new(Vec::new());

enum UpdateEvent {
    Checked(Result<Option<Release>, String>, bool),
    Downloaded(Release, Result<std::path::PathBuf, String>, bool),
}

fn post_update(host: isize, ev: UpdateEvent) {
    if let Ok(mut g) = UPDATE_EVENTS.lock() {
        g.push(ev);
    }
    unsafe { PostMessageW(host as HWND, WM_UPDATE, 0, 0) };
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

fn hash_of<T: Hash>(v: &T) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

struct App {
    host: HWND,
    tray: Tray,
    widgets: Vec<Widget>,
    settings: Option<SettingsWindow>,
    /// 사용량·계정·로그인·업데이트·설정 화면 처리 (macOS와 같은 usagi-core 코드)
    ctl: Controller,
    anim: Animator,
    ink: Rgb,
    icon_size: u32,
    taskbar_created: u32,
    hook: HWINEVENTHOOK,
    frame_ms: u32,
    last_control_tick: f64,
    /// 위젯 내용을 다시 계산해야 함 (사용량·설정이 바뀜). 아니면 1초에 한 번만.
    widget_dirty: bool,
    last_widget: f64,
    /// 설정 창에 마지막으로 보낸 위젯 미리 보기 (같으면 다시 보내지 않음). 0 = 보낸 적 없음
    preview_sent: std::cell::Cell<u64>,
}

impl App {
    // -------------------------------------------------------------- 설정값

    fn mode(&self) -> ValueMode {
        if self.ctl.time_mode() {
            ValueMode::Remaining
        } else {
            ValueMode::Percent
        }
    }

    /// 그래프별 기준 (따로 정하지 않았으면 전체 기준)
    fn meter_left(&self, key: &str) -> bool {
        usagi_core::ui::meter_left(self.ctl.state(), key)
    }

    /// 이 그래프를 위젯에 표시할지 (기본 켜짐, 그래프마다 끌 수 있음)
    fn meter_in_widget(&self, key: &str) -> bool {
        usagi_core::ui::meter_in_widget(self.ctl.state(), key)
    }

    fn widget_on(&self) -> bool {
        self.ctl.state().pref_bool("widget", true)
    }

    fn side(&self) -> Side {
        if self.ctl.state().pref_str("side", "right") == "left" {
            Side::Left
        } else {
            Side::Right
        }
    }

    /// "primary" | "all" | 모니터 장치 이름(\\.\DISPLAY2)
    fn monitor_pref(&self) -> String {
        self.ctl.state().pref_str("monitor", "primary")
    }

    // -------------------------------------------------------------- 위젯 (모니터별)

    /// 설정한 모니터의 작업 표시줄마다 위젯 하나. 모니터가 빠지면 주 모니터로.
    fn sync_widgets(&mut self) {
        let bars = win::taskbars();
        let pref = self.monitor_pref();
        let mut targets: Vec<&win::Taskbar> = match pref.as_str() {
            "all" => bars.iter().collect(),
            "primary" => bars.iter().filter(|b| b.primary).collect(),
            device => bars.iter().filter(|b| b.device.eq_ignore_ascii_case(device)).collect(),
        };
        if targets.is_empty() {
            targets = bars.iter().filter(|b| b.primary).collect();
        }
        // 더는 대상이 아닌 위젯 정리
        self.widgets.retain(|w| {
            let keep = targets.iter().any(|t| t.hwnd == w.taskbar);
            if !keep {
                unsafe { DestroyWindow(w.hwnd) };
            }
            keep
        });
        for t in targets {
            if !self.widgets.iter().any(|w| w.taskbar == t.hwnd) {
                if let Some(hwnd) = create_widget_window() {
                    self.widgets.push(Widget::new(hwnd, t.hwnd, t.primary));
                    self.widget_dirty = true;
                }
            }
        }
        let (on, side) = (self.widget_on(), self.side());
        let bar = self.ctl.state().pref_f64("barPx").map(|v| v as f32);
        let bars = self.ctl.state().pref_bool("bars", true);
        for w in &mut self.widgets {
            w.enabled = on;
            w.side = side;
            w.bar_px = bar;
            w.bars = bars;
            w.reposition();
        }
        // 위젯을 꺼 뒀으면 작업 표시줄 버튼 위치를 물어볼 필요가 없음
        crate::uia::set_active(on && !self.widgets.is_empty());
    }

    fn reposition_widgets(&mut self) {
        for w in &mut self.widgets {
            w.reposition();
        }
    }

    // -------------------------------------------------------------- 그리기

    /// 위젯에 보일 내용: 계정마다 켜 둔 그래프의 라벨·값·비율
    fn widget_sections(&self, now: f64) -> Vec<Section> {
        self
            .ctl
            .snap
            .as_ref()
            .map(|s| {
                s.visible()
                    .filter(|p| !p.meters.is_empty() || s.logged_out(&p.account_id))
                    .map(|p| {
                        // 로그아웃: 예전 값 대신 "로그인 필요"
                        if s.logged_out(&p.account_id) {
                            return Section { id: p.id, key: p.account_id.clone(), rows: Vec::new(), note: Some(texts::login_needed().into()) };
                        }
                        let rows: Vec<Row> = p
                            .meters
                            .iter()
                            .filter(|m| self.meter_in_widget(&usagi_core::engine::alert_key(&p.account_id, &m.label)))
                            .map(|m| {
                                let left = self.meter_left(&usagi_core::engine::alert_key(&p.account_id, &m.label));
                                let percent = m.exact && self.mode() == ValueMode::Percent;
                                let value = if percent {
                                    texts::fmt_usage(m.ratio, left)
                                } else {
                                    texts::meter_value(m, self.mode(), now)
                                };
                                // "Fable 주간" → "Fable" (위젯 라벨 칸이 좁음). 나머지는 언어에 맞춘 화면 이름 (m.label은 저장 키 그대로 key로)
                                let label = match m.label.strip_suffix(" 주간") {
                                    Some(model) => model.to_string(),
                                    None => texts::meter_label(&m.label),
                                };
                                let short = (!percent && self.mode() == ValueMode::Remaining)
                                    .then(|| m.resets_at.filter(|&r| m.exact && r > now).map(|r| texts::fmt_hm_short(r - now)))
                                    .flatten();
                                Row { key: m.label.clone(), label, value, ratio: if m.exact { m.ratio } else { 0.0 }, left, percent, short }
                            })
                            .collect();
                        Section { id: p.id, key: p.account_id.clone(), rows, note: None }
                    })
                    .filter(|s| !s.rows.is_empty() || s.note.is_some()) // 그래프를 모두 끈 계정은 위젯에서 빠짐
                    .collect()
            })
            .unwrap_or_default()
    }

    /// 설정 창에 보일 위젯 미리 보기: 지금 계정·값·설정대로 그린 그림을 작업 표시줄 색 위에 합성.
    /// 지난번에 보낸 것과 같으면 다시 보내지 않음 ({keep: true}), 보일 게 없으면 None
    fn widget_preview(&self) -> Option<serde_json::Value> {
        if !self.ctl.wants_widget_preview() {
            self.preview_sent.set(0);
            return None;
        }
        let sections = self.widget_sections(time::now());
        if sections.is_empty() {
            self.preview_sent.set(0);
            return None;
        }
        let bars = self.ctl.state().pref_bool("bars", true);
        let bar_px = self.ctl.state().pref_f64("barPx").map(|v| v as f32);
        let scale = win::taskbar_dpi() as f32 / 96.0;
        let light = win::taskbar_is_light();
        let content = Content { sections, rabbit: None, ink: self.ink, time_mode: self.mode() == ValueMode::Remaining };
        let key = hash_of(&(format!("{content:?}"), bars, bar_px.map(|v| v as i32), light, (scale * 100.0) as i32)) | 1;
        if self.preview_sent.get() == key {
            return Some(json!({ "keep": true }));
        }
        self.preview_sent.set(key);
        Some(crate::widget::preview_json(&content, bars, bar_px, scale, light))
    }

    fn frame(&mut self) {
        let now = time::now();
        let pose = self.anim.frame(now);
        // 알림 영역 토끼: 로고 색 설정을 따름 (자동이면 작업 표시줄 색). 위젯 글자는 아래 ink(작업 표시줄 색) 그대로
        let (ink, size, tray_ink) = (self.ink, self.icon_size, self.logo_ink());
        let key = hash_of(&(pose, tray_ink, size));
        self.tray.set_icon(key, || (rabbit::render(&pose, size), size, tray_ink));
        if !self.tray.is_added() {
            self.schedule_retry();
        }
        // 위젯: 값이 차오르는 중이거나 내용이 바뀌었을 때만 매 틱 다시 계산, 평소엔 1초에 한 번 (남은 시간 글자)
        let enabled = self.widgets.iter().any(|w| w.enabled);
        let animating = self.widgets.iter().any(|w| w.enabled && w.animating());
        if enabled && (self.widget_dirty || animating || now - self.last_widget >= 1.0) {
            self.widget_dirty = false;
            self.last_widget = now;
            let sections = self.widget_sections(now);
            let content = Content { sections, rabbit: None, ink, time_mode: self.mode() == ValueMode::Remaining };
            for w in self.widgets.iter_mut().filter(|w| w.enabled) {
                w.update(content.clone());
            }
        }
        let frame_ms = if self.widgets.iter().any(|w| w.enabled && w.animating()) {
            FAST_FRAME_MS
        } else {
            let control = if self.ctl.busy() { 0.2 } else { 1.0 };
            let widget = if enabled { self.last_widget + 1.0 - now } else { 1.0 };
            (self.anim.frame_delay(now).min(control).min(widget) * 1000.0).ceil().clamp(10.0, 1000.0) as u32
        };
        if frame_ms != self.frame_ms {
            self.frame_ms = frame_ms;
            unsafe { SetTimer(self.host, TIMER_ANIM, frame_ms, None) };
        }
    }

    /// 알림: Windows 알림(토스트)으로, 설정에 따라 방해 금지 중에도 보이는 중요 알림. 안 되면 트레이 풍선.
    fn notify(&mut self, title: &str, message: &str) {
        let urgent = self.ctl.state().pref_bool("urgent", true);
        if crate::toast::show(title, message, urgent, (self.host, WM_SHOW_SETTINGS)).is_err() {
            self.tray.notify(title, message);
        }
    }

    // -------------------------------------------------------------- 컨트롤러가 시킨 일

    /// 사용량·설정이 바뀜 → 위젯을 바로 다시 그림
    fn redraw(&mut self) {
        self.widget_dirty = true;
        self.frame();
    }

    /// 끝내기: 메시지 루프 밖에서 창을 닫게 (APP을 빌린 채로 DestroyWindow하면 WM_DESTROY를 못 받음)
    fn quit(&self) {
        unsafe { PostMessageW(self.host, WM_CLOSE, 0, 0) };
    }

    /// 공통 컨트롤러(usagi-core)가 돌려준 일을 실행. 설정 화면은 마지막에 한 번만 다시 보냄.
    fn run(&mut self, effects: Vec<Effect>) {
        let mut queue: VecDeque<Effect> = effects.into();
        let mut push = false;
        while let Some(e) = queue.pop_front() {
            match e {
                Effect::Push => push = true,
                Effect::Redraw => self.redraw(),
                Effect::PlatformPref(key) => {
                    if matches!(key.as_str(), "widget" | "side" | "monitor" | "barPx" | "bars") {
                        self.sync_widgets();
                    }
                    if key == "logo" {
                        // 알림 영역 토끼·풍선·알림 아이콘을 바로 새 색으로 (다음 틱을 기다리지 않음)
                        crate::toast::refresh_icon();
                        self.tray.set_balloon_icon(&rabbit::render(&Pose::default(), 64), 64, self.logo_ink());
                        self.frame();
                    }
                    if key == "locale" {
                        // 트레이 메뉴는 열 때마다 새로 만들고, 위젯은 뒤따르는 Redraw로 다시 그림. 툴팁만 여기서 바꿈
                        self.tray.set_tip(&texts::tooltip(self.ctl.snap.as_ref()));
                    }
                }
                Effect::Open(target) => shell_open(&target),
                Effect::OpenText(path) => shell_open(&path.to_string_lossy()),
                Effect::OpenNotifySettings => shell_open("ms-settings:notifications"),
                Effect::Notify(title, body) => self.notify(&title, &body),
                Effect::SetAutostart(on) => {
                    if let Err(error) = crate::autostart::set(on) {
                        self.ctl.set_error(error);
                    }
                }
                Effect::CloseSettings => {
                    self.settings = None;
                    queue.extend(self.ctl.settings_closed());
                }
                Effect::Quit => {
                    self.quit();
                    return;
                }
                Effect::CheckUpdate { manual } => {
                    let host = self.host as isize;
                    std::thread::spawn(move || post_update(host, UpdateEvent::Checked(crate::updater::check(), manual)));
                }
                Effect::DownloadUpdate { release, install } => {
                    let host = self.host as isize;
                    std::thread::spawn(move || {
                        let r = crate::updater::download(&release);
                        post_update(host, UpdateEvent::Downloaded(release, r, install));
                    });
                }
                Effect::ApplyUpdate(staged) => match crate::updater::apply(&staged) {
                    Ok(()) => {
                        self.quit();
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

    fn schedule_retry(&mut self) {
        if !self.tray.retry_pending {
            self.tray.retry_pending = true;
            unsafe { SetTimer(self.host, TIMER_RETRY_ADD, 2000, None) };
        }
    }

    /// 로고 색 설정: 흰색 / 검은색 / 자동(작업 표시줄 색)
    fn logo_ink(&self) -> win::Rgb {
        match self.ctl.state().pref_str("logo", "auto").as_str() {
            "white" => win::INK_ON_DARK,
            "ink" => win::INK_ON_LIGHT,
            _ => self.ink,
        }
    }

    fn recheck_display(&mut self) {
        let (ink, size) = (win::theme_ink(), win::small_icon_size());
        if ink != self.ink || size != self.icon_size {
            self.ink = ink;
            self.icon_size = size;
            self.tray.clear_cache();
            self.tray.set_balloon_icon(&rabbit::render(&Pose::default(), 64), 64, self.logo_ink());
            crate::toast::refresh_icon(); // 라이트·다크 모드가 바뀌면 알림 아이콘 색도
        }
        self.sync_widgets();
        crate::uia::poke();
        self.redraw();
        self.push_settings();
    }

    fn apply(&mut self, snap: Snapshot) {
        let now = time::now();
        if let Some(st) = &snap.claude {
            self.anim.set(st, st.session_pct, st.weekly_pct, now);
        }
        self.tray.set_tip(&texts::tooltip(Some(&snap)));
        let fx = self.ctl.apply_snapshot(snap);
        self.run(fx);
    }

    // -------------------------------------------------------------- 설정 창 (화면 상태·명령 처리는 usagi-core 컨트롤러)

    fn open_settings(&mut self) {
        if let Some(s) = &self.settings {
            s.focus();
            return;
        }
        match settings::open(self.host) {
            Ok(s) => {
                self.settings = Some(s);
                // 새 설정 창에는 미리 보기 그림을 처음부터 다시 보냄
                self.preview_sent.set(0);
                self.ctl.settings_opened();
            }
            Err(e) => unsafe {
                MessageBoxW(self.host, wide(&e.to_string()).as_ptr(), wide("usAgI").as_ptr(), MB_ICONWARNING);
            },
        }
    }

    /// Windows에서 읽는 값: 시작 시 실행, 중요 알림 허용, 모니터 목록
    fn host_info(&self) -> HostInfo {
        let monitors = win::monitors()
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let (n, w, h) = (i + 1, m.rect.right - m.rect.left, m.rect.bottom - m.rect.top);
                let label = usagi_core::tr!(
                    format!("모니터 {n} · {} · {w}×{h}{}", m.name, if m.primary { " (주 모니터)" } else { "" }),
                    format!("Monitor {n} · {} · {w}×{h}{}", m.name, if m.primary { " (primary)" } else { "" })
                );
                json!({
                    "id": m.device,
                    "label": label,
                })
            })
            .collect();
        HostInfo {
            taskbar_light: Some(win::taskbar_is_light()),
            widget_preview: self.widget_preview(),
            bar_range: Some(crate::widget::bar_range()).map(|(a, b, c)| (a.round() as f64, b.round() as f64, c.round() as f64)),
            autostart: crate::autostart::enabled(),
            urgent_supported: crate::toast::urgent_supported(),
            urgent_allowed: crate::toast::urgent_allowed(),
            monitors,
        }
    }

    fn push_settings(&self) {
        if let Some(s) = &self.settings {
            s.push(self.ctl.settings_state(&self.host_info()).to_string());
        }
    }

    // -------------------------------------------------------------- 트레이 메뉴

    /// 누를 때마다 새로 만듦 (언어를 바꿔도 다음에 열 때 바로 반영)
    fn tray_menu(&mut self) {
        let items = [
            (1u32, usagi_core::tr!("설정 열기", "Open settings")),
            (4, usagi_core::tr!("작업 표시줄 위젯 표시", "Show taskbar widget")),
            (2, usagi_core::tr!("지금 새로고침", "Refresh now")),
            (0, ""),
            (3, usagi_core::tr!("usAgI 종료", "Quit usAgI")),
        ];
        let widget_on = self.widget_on();
        let cmd = unsafe {
            let menu = CreatePopupMenu();
            for (id, text) in items {
                if id == 0 {
                    AppendMenuW(menu, MF_SEPARATOR, 0, null());
                } else {
                    let checked = if id == 4 && widget_on { MF_CHECKED } else { MF_UNCHECKED };
                    AppendMenuW(menu, MF_STRING | checked, id as usize, wide(text).as_ptr());
                }
            }
            let mut pt = POINT { x: 0, y: 0 };
            GetCursorPos(&mut pt);
            SetForegroundWindow(self.host);
            let cmd = TrackPopupMenuEx(menu, TPM_RIGHTBUTTON | TPM_NONOTIFY | TPM_RETURNCMD | TPM_BOTTOMALIGN, pt.x, pt.y, self.host, null());
            PostMessageW(self.host, WM_NULL, 0, 0);
            DestroyMenu(menu);
            cmd as u32
        };
        match cmd {
            1 => self.open_settings(),
            2 => self.ctl.refresh_all(),
            4 => {
                let fx = self.ctl.set_pref("widget", json!(!widget_on));
                self.run(fx);
            }
            3 => self.quit(),
            _ => {}
        }
    }

    // -------------------------------------------------------------- 메시지

    fn handle(&mut self, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        if self.widgets.iter().any(|w| w.hwnd == hwnd) {
            return match msg {
                WM_MOUSEACTIVATE => Some(MA_NOACTIVATE as LRESULT),
                WM_LBUTTONDBLCLK => {
                    self.open_settings();
                    Some(0)
                }
                WM_RBUTTONUP => {
                    // 퍼센트 ↔ 남은 시간
                    let fx = self.ctl.toggle_mode();
                    self.run(fx);
                    Some(0)
                }
                WM_RBUTTONDOWN | WM_CONTEXTMENU => Some(0),
                _ => None,
            };
        }
        if hwnd != self.host {
            return None;
        }
        match msg {
            WM_TIMER => {
                match wp {
                    TIMER_ANIM => {
                        self.frame();
                        let now = time::now();
                        let interval = if self.ctl.busy() { 0.2 } else { 1.0 };
                        if now - self.last_control_tick >= interval {
                            self.last_control_tick = now;
                            let fx = self.ctl.tick();
                            self.run(fx);
                        }
                    }
                    TIMER_PLACE => self.sync_widgets(),
                    TIMER_RECHECK => {
                        unsafe { KillTimer(self.host, TIMER_RECHECK) };
                        self.recheck_display();
                    }
                    TIMER_RETRY_ADD => {
                        unsafe { KillTimer(self.host, TIMER_RETRY_ADD) };
                        self.tray.retry_pending = false;
                        if !self.tray.is_added() && !self.tray.add() {
                            self.schedule_retry();
                        }
                    }
                    _ => {}
                }
                Some(0)
            }
            WM_UPDATE => {
                let events: Vec<UpdateEvent> = UPDATE_EVENTS.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default();
                for ev in events {
                    let fx = match ev {
                        UpdateEvent::Checked(r, manual) => self.ctl.update_checked(r, manual),
                        UpdateEvent::Downloaded(rel, r, install) => self.ctl.update_downloaded(rel, r, install),
                    };
                    self.run(fx);
                }
                Some(0)
            }
            WM_SNAPSHOT => {
                if let Some(s) = LATEST.lock().ok().and_then(|mut g| g.take()) {
                    self.apply(s);
                }
                Some(0)
            }
            WM_IPC => {
                while let Some(m) = self.settings.as_ref().and_then(SettingsWindow::next_command) {
                    let fx = self.ctl.handle_ipc(&m);
                    self.run(fx);
                }
                Some(0)
            }
            WM_FOREGROUND => {
                // 앞 창이 바뀜: 작업 표시줄 버튼이 늘거나 줄었을 수 있으니 곧 다시 확인
                crate::uia::poke();
                self.reposition_widgets();
                Some(0)
            }
            WM_SHOW_SETTINGS => {
                self.open_settings();
                Some(0)
            }
            WM_TRAY => {
                // NOTIFYICON_VERSION_4: LOWORD(lParam) = 이벤트
                match (lp & 0xFFFF) as u32 {
                    WM_CONTEXTMENU => self.tray_menu(),
                    WM_LBUTTONDBLCLK => self.open_settings(),
                    _ => {}
                }
                Some(0)
            }
            WM_DISPLAYCHANGE | WM_SETTINGCHANGE | WM_DPICHANGED => {
                unsafe { SetTimer(self.host, TIMER_RECHECK, 700, None) };
                None
            }
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                Some(0)
            }
            m if m == self.taskbar_created && m != 0 => {
                // 작업 표시줄이 새로 만들어짐 (탐색기 재시작, 모니터/DPI 변경) → 같은 ID로 다시 등록
                if !self.tray.readd() {
                    self.schedule_retry();
                }
                unsafe { SetTimer(self.host, TIMER_RECHECK, 300, None) };
                Some(0)
            }
            _ => None,
        }
    }
}

impl Drop for App {
    fn drop(&mut self) {
        self.ctl.engine.stop();
        self.settings = None;
        if !self.hook.is_null() {
            unsafe { UnhookWinEvent(self.hook) };
        }
        for w in &self.widgets {
            unsafe { DestroyWindow(w.hwnd) };
        }
    }
}

// ------------------------------------------------------------------ Win32 콜백

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let handled = APP.with(|cell| match cell.try_borrow_mut() {
        Ok(mut guard) => guard.as_mut().and_then(|app| app.handle(hwnd, msg, wp, lp)),
        Err(_) => None, // 메뉴/웹뷰 생성 등으로 APP이 사용 중 → 기본 처리
    });
    match handled {
        Some(r) => r,
        None => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn on_foreground(_: HWINEVENTHOOK, _: u32, _: HWND, _: i32, _: i32, _: u32, _: u32) {
    let host = HOST.load(Ordering::Relaxed);
    if host != 0 {
        PostMessageW(host as HWND, WM_FOREGROUND, 0, 0);
    }
}

fn register_class(name: &str, style: u32) {
    unsafe {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style,
            lpfnWndProc: Some(wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: GetModuleHandleW(null()),
            hIcon: LoadIconW(GetModuleHandleW(null()), 1 as *const u16),
            hCursor: LoadCursorW(null_mut(), IDC_ARROW),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: wide(name).leak().as_ptr(),
            hIconSm: LoadImageW(GetModuleHandleW(null()), 1 as *const u16, IMAGE_ICON, GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON), LR_DEFAULTCOLOR) as _,
        };
        RegisterClassExW(&wc);
    }
}

fn create_widget_window() -> Option<HWND> {
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            wide(WIDGET_CLASS).as_ptr(),
            wide("usAgI widget").as_ptr(),
            WS_POPUP,
            0,
            0,
            1,
            1,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        )
    };
    (!hwnd.is_null()).then_some(hwnd)
}

// ------------------------------------------------------------------ 도우미

fn shell_open(path: &str) {
    unsafe {
        ShellExecuteW(null_mut(), wide("open").as_ptr(), wide(path).as_ptr(), null(), null(), SW_SHOWNORMAL);
    }
}

// ------------------------------------------------------------------ 시작

/// `from_autostart`: 로그인할 때 자동 실행으로 시작됨 (이미 떠 있으면 설정 창을 열지 않고 끝냄)
pub fn run(from_autostart: bool) {
    unsafe {
        // 중복 실행: 직접 실행했으면 이미 떠 있는 usAgI의 설정 창을 엶, 자동 실행이면 조용히 끝냄
        CreateMutexW(null(), 0, wide("Local\\usAgI-app").as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            if !from_autostart {
                let host = FindWindowW(wide(HOST_CLASS).as_ptr(), null());
                if !host.is_null() {
                    PostMessageW(host, WM_SHOW_SETTINGS, 0, 0);
                }
            }
            return;
        }
        win::enable_dpi_awareness();
        OleInitialize(null_mut());
    }

    register_class(HOST_CLASS, 0);
    register_class(WIDGET_CLASS, CS_DBLCLKS);

    let host = unsafe {
        // 방송 메시지(TaskbarCreated 등)를 받으려면 메시지 전용 창이 아닌 최상위 창이어야 함
        CreateWindowExW(WS_EX_TOOLWINDOW, wide(HOST_CLASS).as_ptr(), wide("usAgI").as_ptr(), WS_POPUP, 0, 0, 0, 0, null_mut(), null_mut(), GetModuleHandleW(null()), null())
    };
    HOST.store(host as isize, Ordering::Relaxed);

    let engine = Engine::start(|snap| {
        if let Ok(mut g) = LATEST.lock() {
            *g = Some(snap);
        }
        let host = HOST.load(Ordering::Relaxed);
        if host != 0 {
            unsafe { PostMessageW(host as HWND, WM_SNAPSHOT, 0, 0) };
        }
    });

    // Windows 알림용 앱 이름·아이콘 등록 (설정 > 알림 목록에 usAgI로 보임)
    {
        crate::toast::refresh_icon();
        // 설치형일 때만 시작 메뉴 바로 가기 (포터블은 흔적을 남기지 않음)
        if !config::is_portable() {
            if let Ok(exe) = std::env::current_exe() {
                let _ = crate::toast::ensure_shortcut(&exe);
            }
        }
    }
    let ink = win::theme_ink();
    let mut tray = Tray::new(host);
    tray.set_balloon_icon(&rabbit::render(&Pose::default(), 64), 64, ink);
    let hook = unsafe { SetWinEventHook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND, null_mut(), Some(on_foreground), 0, 0, WINEVENT_OUTOFCONTEXT) };
    let seed = (time::now() * 1000.0) as u64;

    let app = App {
        host,
        tray,
        widgets: Vec::new(),
        settings: None,
        preview_sent: std::cell::Cell::new(0),
        ctl: Controller::new(engine, Platform { name: "windows", repo: crate::updater::REPO, managed_update: None }),
        anim: Animator::new(seed),
        ink,
        icon_size: win::small_icon_size(),
        taskbar_created: unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) },
        hook,
        frame_ms: 0,
        last_control_tick: 0.0,
        widget_dirty: true,
        last_widget: 0.0,
    };
    APP.with(|cell| *cell.borrow_mut() = Some(app));
    APP.with(|cell| {
        if let Some(app) = cell.borrow_mut().as_mut() {
            app.tray.set_tip(usagi_core::tr!("usAgI · 불러오는 중…", "usAgI · Loading…"));
            app.sync_widgets();
            app.frame();
            crate::autostart::refresh(); // 자동 실행이 켜져 있으면 지금 위치로 로그온 작업을 맞춤 (예전 Run 등록은 옮김)
            let fx = app.ctl.startup();
            app.run(fx);
            if !from_autostart && usagi_core::accounts::load(app.ctl.state()).is_empty() {
                app.open_settings();
            }
        }
    });
    unsafe {
        SetTimer(host, TIMER_PLACE, 1000, None);
    }

    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    HOST.store(0, Ordering::Relaxed);
    APP.with(|cell| cell.borrow_mut().take()); // 트레이 아이콘 제거, 엔진 정지
}
