//! 작업 표시줄의 앱 버튼과 알림 영역 좌표.
//!
//! UI Automation은 짧게 실행되는 usagi.exe의 별도 모드에서만 사용.
//! 본체는 좌표를 캐시하고 작업 표시줄 변경 때 다시 조회.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::UI::Accessibility::{
    AutomationElementMode_None, CUIAutomation, IUIAutomation, IUIAutomationCacheRequest, TreeScope_Descendants, UIA_BoundingRectanglePropertyId,
    UIA_ClassNamePropertyId,
};

pub const HELPER_ARG: &str = "--taskbar-extents";
// 15초마다 작업 표시줄 지문(위치·크기·배율·버튼이 있는 창 목록)만 가볍게 확인하고, 바뀌었을 때만 도우미 실행.
// XAML 변경 이벤트가 빠질 수 있어 지문이 같아도 60초마다 한 번은 실제로 다시 잼.
const IDLE: Duration = Duration::from_secs(15);
const FORCE: Duration = Duration::from_secs(60);
const QUICK: Duration = Duration::from_millis(1200);
const SETTLE: Duration = Duration::from_millis(250);
const FRESH: Duration = Duration::from_secs(90);

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Extents {
    pub buttons_right: Option<i32>,
    pub tray_left: Option<i32>,
}

#[derive(Default)]
struct Cache {
    targets: Vec<isize>,
    generation: u64,
    measured_at: Option<Instant>,
    extents: HashMap<isize, Extents>,
}

static CACHE: LazyLock<Mutex<Cache>> = LazyLock::new(|| Mutex::new(Cache::default()));
static WAKE: OnceLock<SyncSender<()>> = OnceLock::new();
/// 지문과 상관없이 다음 확인 때 꼭 다시 잼 (탐색기가 작업 표시줄 배치를 바꿈, 화면 구성 변경, 대상 변경)
static FORCE_NEXT: AtomicBool = AtomicBool::new(false);

pub fn get(taskbar: isize) -> Option<Extents> {
    let cache = CACHE.lock().ok()?;
    cache.measured_at.filter(|at| at.elapsed() < FRESH)?;
    cache.extents.get(&taskbar).copied()
}

pub fn is_active() -> bool {
    CACHE.lock().is_ok_and(|cache| !cache.targets.is_empty())
}

/// 앞 창이 바뀜: 작업 표시줄 버튼이 늘거나 줄었을 수 있음 → 지문이 바뀌었으면 다시 잼
pub fn poke() {
    if is_active() {
        wake();
    }
}

/// 탐색기가 작업 표시줄 안의 창을 옮겼거나 화면 구성이 바뀜 → 지문과 상관없이 다시 잼
pub fn poke_layout() {
    if is_active() {
        FORCE_NEXT.store(true, Ordering::Relaxed);
        wake();
    }
}

fn wake() {
    if let Some(tx) = WAKE.get() {
        let _ = tx.try_send(());
    }
}

/// 오른쪽에 위젯을 배치하는 작업 표시줄만 조회.
pub fn set_taskbars(targets: Vec<isize>) {
    if let Ok(mut cache) = CACHE.lock() {
        if cache.targets == targets {
            return;
        }
        cache.generation = cache.generation.wrapping_add(1);
        cache.extents.retain(|key, _| targets.contains(key));
        cache.targets = targets;
    }
    if is_active() {
        WAKE.get_or_init(start);
    }
    FORCE_NEXT.store(true, Ordering::Relaxed);
    wake();
}

fn start() -> SyncSender<()> {
    let (tx, rx) = mpsc::sync_channel::<()>(1);
    let _ = std::thread::Builder::new().name("usagi-taskbar".into()).spawn(move || {
        let mut quick = true;
        // 마지막으로 실제로 잰 때의 지문·시각. 바뀐 걸 보고 잰 뒤에는 버튼 애니메이션이 끝난 1.2초 뒤에 한 번 더 잼
        let mut measured: Option<(u64, Instant)> = None;
        let mut follow_up = false;
        loop {
            let Ok((generation, targets)) = CACHE.lock().map(|cache| (cache.generation, cache.targets.clone())) else { return };
            if targets.is_empty() {
                if rx.recv().is_err() {
                    return;
                }
                quick = true;
                measured = None;
                continue;
            }
            let print = fingerprint(&targets);
            let forced = FORCE_NEXT.swap(false, Ordering::Relaxed);
            let changed = measured.is_none_or(|(old, _)| old != print);
            let stale = measured.is_none_or(|(_, at)| at.elapsed() >= FORCE);
            let was_follow_up = std::mem::take(&mut follow_up);
            if forced || changed || stale || was_follow_up {
                match helper_output(&targets) {
                    Some(extents) => {
                        if let Ok(mut cache) = CACHE.lock() {
                            if cache.generation == generation {
                                cache.extents = extents;
                                cache.measured_at = Some(Instant::now());
                            }
                        }
                        // 바뀐 걸 보고 쟀으면 1.2초 뒤 한 번 더 (방금 잰 값이 버튼 애니메이션 중간일 수 있음)
                        if (forced || changed) && !was_follow_up {
                            follow_up = true;
                            quick = true;
                        }
                        measured = Some((print, Instant::now()));
                    }
                    None => measured = None, // 실패: 다음 확인 때 다시 시도
                }
            } else if let Ok(mut cache) = CACHE.lock() {
                // 지문이 그대로: 도우미를 띄우지 않고 지금 좌표를 계속 사용
                if cache.generation == generation && !cache.extents.is_empty() {
                    cache.measured_at = Some(Instant::now());
                }
            }
            let delay = if std::mem::take(&mut quick) { QUICK } else { IDLE };
            match rx.recv_timeout(delay) {
                Ok(()) => {
                    std::thread::sleep(SETTLE);
                    while rx.try_recv().is_ok() {}
                    quick = true;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });
    tx
}

/// 작업 표시줄 지문: 대상 작업 표시줄과 그 안의 알림 영역·버튼 영역·시작 버튼 위치, 화면 배율,
/// 그리고 작업 표시줄에 버튼이 생기는 창 목록. 이 값이 같으면 버튼 끝·알림 영역 위치도 같다고 봄 (도우미 실행 생략)
fn fingerprint(targets: &[isize]) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED};
    use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    fn rect(hwnd: HWND) -> Option<(i32, i32, i32, i32)> {
        crate::win::window_rect(hwnd).map(|r| (r.left, r.top, r.right, r.bottom))
    }
    let mut h = DefaultHasher::new();
    for &tb in targets {
        let hwnd = tb as HWND;
        rect(hwnd).hash(&mut h);
        unsafe { GetDpiForWindow(hwnd) }.hash(&mut h);
        for class in ["TrayNotifyWnd", "ReBarWindow32", "WorkerW", "Start"] {
            let child = unsafe { FindWindowExW(hwnd, std::ptr::null_mut(), crate::win::wide(class).as_ptr(), std::ptr::null()) };
            rect(child).hash(&mut h);
        }
    }
    // 버튼이 생기는 창: 보이고, 주인 창이 없거나 앱 창 표시가 있고, 도구 창이 아니고, 숨겨지지(cloaked) 않은 창
    unsafe extern "system" fn each(hwnd: HWND, list: LPARAM) -> BOOL {
        let list = &mut *(list as *mut Vec<isize>);
        if IsWindowVisible(hwnd) == 0 {
            return 1;
        }
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let owned = !GetWindow(hwnd, GW_OWNER).is_null();
        if ex & WS_EX_TOOLWINDOW != 0 || (owned && ex & WS_EX_APPWINDOW == 0) {
            return 1;
        }
        let mut cloaked = 0u32;
        let ok = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED as u32, (&mut cloaked as *mut u32).cast(), 4) >= 0;
        if ok && cloaked != 0 {
            return 1;
        }
        list.push(hwnd as isize);
        1
    }
    let mut windows: Vec<isize> = Vec::new();
    unsafe { EnumWindows(Some(each), &mut windows as *mut Vec<isize> as LPARAM) };
    windows.sort_unstable();
    windows.hash(&mut h);
    h.finish()
}

fn helper_output(targets: &[isize]) -> Option<HashMap<isize, Extents>> {
    let mut cmd = Command::new(std::env::current_exe().ok()?);
    cmd.arg(HELPER_ARG).args(targets.iter().map(isize::to_string)).creation_flags(0x0800_0000);
    let output = usagi_core::login::run_bounded_command(cmd, "", Duration::from_secs(4))?;
    serde_json::from_str(&output).ok()
}

pub fn run_helper(args: &[String]) -> std::io::Result<()> {
    unsafe {
        crate::win::enable_dpi_awareness();
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().map_err(std::io::Error::other)?;
    }
    let result = collect_extents(args);
    unsafe { CoUninitialize() };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer(&mut out, &result?)?;
    out.flush()
}

fn collect_extents(args: &[String]) -> std::io::Result<HashMap<isize, Extents>> {
    let requested: Vec<isize> = args.iter().filter_map(|arg| arg.parse().ok()).collect();
    // Validate current shell handles again in case Explorer restarted.
    unsafe {
        let uia: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).map_err(std::io::Error::other)?;
        let cache = uia.CreateCacheRequest().ok().filter(|c| {
            c.AddProperty(UIA_BoundingRectanglePropertyId).is_ok()
                && c.AddProperty(UIA_ClassNamePropertyId).is_ok()
                && c.SetAutomationElementMode(AutomationElementMode_None).is_ok()
        });
        Ok(crate::win::taskbars().into_iter().filter_map(|bar| {
            let hwnd = bar.hwnd as isize;
            requested.contains(&hwnd).then(|| probe(&uia, cache.as_ref(), hwnd)).flatten().map(|e| (hwnd, e))
        }).collect())
    }
}

unsafe fn probe(uia: &IUIAutomation, cache: Option<&IUIAutomationCacheRequest>, hwnd: isize) -> Option<Extents> {
    let root = uia.ElementFromHandle(windows::Win32::Foundation::HWND(hwnd as _)).ok()?;
    let cond = uia.CreateTrueCondition().ok()?;
    let (mut right, mut tray): (Option<i32>, Option<i32>) = (None, None);
    let mut note = |r: windows::Win32::Foundation::RECT, cls: String| {
        if r.right <= r.left {
            return;
        }
        if cls.contains("TaskListButton") {
            right = Some(right.map_or(r.right, |x| x.max(r.right)));
        } else if cls.starts_with("SystemTray.") {
            tray = Some(tray.map_or(r.left, |x| x.min(r.left)));
        }
    };
    match cache {
        Some(cache) => {
            // 한 번의 요청으로 모든 요소의 값까지
            let all = root.FindAllBuildCache(TreeScope_Descendants, &cond, cache).ok()?;
            for i in 0..all.Length().ok()? {
                let Ok(e) = all.GetElement(i) else { continue };
                let Ok(r) = e.CachedBoundingRectangle() else { continue };
                note(r, e.CachedClassName().map(|b| b.to_string()).unwrap_or_default());
            }
        }
        None => {
            let all = root.FindAll(TreeScope_Descendants, &cond).ok()?;
            for i in 0..all.Length().ok()? {
                let Ok(e) = all.GetElement(i) else { continue };
                let Ok(r) = e.CurrentBoundingRectangle() else { continue };
                note(r, e.CurrentClassName().map(|b| b.to_string()).unwrap_or_default());
            }
        }
    }
    Some(Extents { buttons_right: right, tray_left: tray })
}
