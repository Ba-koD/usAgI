//! 작업 표시줄의 앱 버튼과 알림 영역 좌표.
//!
//! UI Automation은 짧게 실행되는 usagi.exe의 별도 모드에서만 사용.
//! 본체는 좌표를 캐시하고 작업 표시줄 변경 때 다시 조회.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::os::windows::process::CommandExt;
use std::process::Command;
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::UI::Accessibility::{
    AutomationElementMode_None, CUIAutomation, IUIAutomation, IUIAutomationCacheRequest, TreeScope_Descendants, UIA_BoundingRectanglePropertyId,
    UIA_ClassNamePropertyId,
};

pub const HELPER_ARG: &str = "--taskbar-extents";
// XAML 변경 이벤트가 빠질 수 있어 드물게 좌표를 재확인.
const IDLE: Duration = Duration::from_secs(15);
const QUICK: Duration = Duration::from_millis(1200);
const SETTLE: Duration = Duration::from_millis(250);
const FRESH: Duration = Duration::from_secs(35);

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

pub fn get(taskbar: isize) -> Option<Extents> {
    let cache = CACHE.lock().ok()?;
    cache.measured_at.filter(|at| at.elapsed() < FRESH)?;
    cache.extents.get(&taskbar).copied()
}

pub fn is_active() -> bool {
    CACHE.lock().is_ok_and(|cache| !cache.targets.is_empty())
}

pub fn poke() {
    if is_active() {
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
    wake();
}

fn start() -> SyncSender<()> {
    let (tx, rx) = mpsc::sync_channel::<()>(1);
    let _ = std::thread::Builder::new().name("usagi-taskbar".into()).spawn(move || {
        let mut quick = true;
        loop {
            let Ok((generation, targets)) = CACHE.lock().map(|cache| (cache.generation, cache.targets.clone())) else { return };
            if targets.is_empty() {
                if rx.recv().is_err() {
                    return;
                }
                quick = true;
                continue;
            }
            if let Some(extents) = helper_output(&targets) {
                if let Ok(mut cache) = CACHE.lock() {
                    if cache.generation == generation {
                        cache.extents = extents;
                        cache.measured_at = Some(Instant::now());
                    }
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
