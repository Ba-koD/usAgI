//! 작업 표시줄의 실제 버튼 위치 (UI 자동화).
//!
//! Windows 11 작업 표시줄은 XAML로 그려서, 예전 창(MSTaskListWClass·ReBarWindow32)의 위치가
//! 실제 앱 버튼과 다를 수 있어요(특히 보조 모니터·세로 모니터). 그래서 접근성(UI Automation)으로
//! 앱 버튼의 오른쪽 끝과 시계·알림 영역의 왼쪽 끝을 읽어, 위젯이 버튼을 가리지 않게 합니다.
//!
//! 다른 프로세스(explorer)에 묻는 일이라 백그라운드 스레드에서 하고, 가볍게 유지해요:
//! - 필요한 값(위치·클래스 이름)을 캐시 요청으로 **한 번에** 받아 와요 (요소마다 따로 묻지 않음)
//! - 평소엔 6초마다, 창이 바뀌었을 때([`poke`])만 곧바로 몇 번 더 확인해요
//! - 위젯이 하나도 보이지 않으면([`set_active`]) 아예 묻지 않아요

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::UI::Accessibility::{
    AutomationElementMode_None, CUIAutomation, IUIAutomation, IUIAutomationCacheRequest, TreeScope_Descendants, UIA_BoundingRectanglePropertyId,
    UIA_ClassNamePropertyId,
};

/// 평소 확인 간격
const IDLE: Duration = Duration::from_secs(6);
/// 창이 바뀐 직후(작업 표시줄 버튼이 생기거나 사라지는 중) 확인 간격과 횟수
const QUICK: Duration = Duration::from_millis(1200);
const QUICK_ROUNDS: u32 = 2;
/// 변화 직후 작업 표시줄 애니메이션이 끝나길 잠깐 기다림
const SETTLE: Duration = Duration::from_millis(250);
/// 이보다 오래된 값은 쓰지 않음 (IDLE보다 넉넉하게)
const FRESH: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug)]
pub struct Extents {
    /// 앱 버튼들의 오른쪽 끝 (화면 픽셀)
    pub buttons_right: Option<i32>,
    /// 시계·알림 영역의 왼쪽 끝 (화면 픽셀)
    pub tray_left: Option<i32>,
    at: Instant,
}

static EXTENTS: Mutex<Option<HashMap<isize, Extents>>> = Mutex::new(None);
static ACTIVE: AtomicBool = AtomicBool::new(true);
static WAKE: OnceLock<Sender<()>> = OnceLock::new();

/// 최근 값만
pub fn get(taskbar: isize) -> Option<Extents> {
    let g = EXTENTS.lock().ok()?;
    g.as_ref()?.get(&taskbar).copied().filter(|e| e.at.elapsed() < FRESH)
}

/// 작업 표시줄 버튼이 바뀌었을 수 있음 (앞 창이 바뀜, 화면 구성이 바뀜) → 곧 다시 확인
pub fn poke() {
    if let Some(tx) = WAKE.get() {
        let _ = tx.send(());
    }
}

/// 보이는 위젯이 있을 때만 확인
pub fn set_active(on: bool) {
    if ACTIVE.swap(on, Ordering::Relaxed) != on && on {
        poke();
    }
}

pub fn start() {
    let (tx, rx) = mpsc::channel::<()>();
    if WAKE.set(tx).is_err() {
        return; // 이미 시작함
    }
    let _ = std::thread::Builder::new().name("usagi-uia".into()).spawn(move || unsafe {
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            return;
        }
        let Ok(uia) = CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER) else { return };
        // 위치와 클래스 이름을 찾을 때 함께 받아 옴 (요소 참조는 받지 않음)
        let cache = uia.CreateCacheRequest().ok().filter(|c| {
            c.AddProperty(UIA_BoundingRectanglePropertyId).is_ok()
                && c.AddProperty(UIA_ClassNamePropertyId).is_ok()
                && c.SetAutomationElementMode(AutomationElementMode_None).is_ok()
        });
        let mut quick = QUICK_ROUNDS;
        loop {
            if ACTIVE.load(Ordering::Relaxed) {
                let mut found = HashMap::new();
                for tb in crate::win::taskbars() {
                    if let Some(e) = probe(&uia, cache.as_ref(), tb.hwnd as isize) {
                        found.insert(tb.hwnd as isize, e);
                    }
                }
                if let Ok(mut g) = EXTENTS.lock() {
                    *g = Some(found);
                }
            }
            let wait = if quick > 0 {
                quick -= 1;
                QUICK
            } else {
                IDLE
            };
            match rx.recv_timeout(wait) {
                Ok(()) => {
                    std::thread::sleep(SETTLE);
                    while rx.try_recv().is_ok() {} // 몰려 온 신호는 한 번으로
                    quick = QUICK_ROUNDS;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });
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
    Some(Extents { buttons_right: right, tray_left: tray, at: Instant::now() })
}
