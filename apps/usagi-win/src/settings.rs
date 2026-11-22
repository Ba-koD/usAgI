//! 설정 창 — HTML(settings.html)을 시스템 웹뷰(WebView2)로 띄웁니다.
//! 창을 열 때만 웹뷰를 만들고 닫으면 바로 해제하므로 평소 메모리는 늘지 않습니다.
//! JS → Rust: window.ipc.postMessage(JSON)  /  Rust → JS: window.usagi.render(state)

use crate::win::{self, wide};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::num::NonZeroIsize;
use std::ptr::null_mut;
use std::rc::Rc;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForSystem;
use windows_sys::Win32::UI::WindowsAndMessaging::*;
use wry::dpi::{PhysicalPosition, PhysicalSize};
use wry::raw_window_handle::{HandleError, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle};
use wry::{Rect, WebContext, WebView, WebViewBuilder};

pub const WM_IPC: u32 = WM_APP + 11;
pub const CLASS: &str = "usAgISettings";

struct Parent(HWND);

impl HasWindowHandle for Parent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let h = NonZeroIsize::new(self.0 as isize).ok_or(HandleError::Unavailable)?;
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(Win32WindowHandle::new(h))) })
    }
}

pub struct SettingsWindow {
    pub hwnd: HWND,
    webview: Option<WebView>,
    _ctx: WebContext,
}

fn client_rect(hwnd: HWND) -> Rect {
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    unsafe { GetClientRect(hwnd, &mut r) };
    Rect {
        position: PhysicalPosition::new(0, 0).into(),
        size: PhysicalSize::new((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32).into(),
    }
}

impl SettingsWindow {
    pub fn open(host: HWND, inbox: Rc<RefCell<VecDeque<String>>>) -> Result<Self, String> {
        let dark = !win::apps_are_light();
        let scale = unsafe { GetDpiForSystem() } as f32 / 96.0;
        let (w, h) = ((980.0 * scale) as i32, (660.0 * scale) as i32);
        let hwnd = unsafe {
            // 커서가 있는 모니터 가운데
            let mut pt = POINT { x: 0, y: 0 };
            GetCursorPos(&mut pt);
            let mut mi: MONITORINFO = std::mem::zeroed();
            mi.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            GetMonitorInfoW(MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST), &mut mi);
            let wa = mi.rcWork;
            let x = wa.left + ((wa.right - wa.left) - w) / 2;
            let y = wa.top + ((wa.bottom - wa.top) - h).max(0) / 2;
            CreateWindowExW(
                0,
                wide(CLASS).as_ptr(),
                wide("usAgI 설정").as_ptr(),
                WS_OVERLAPPEDWINDOW,
                x,
                y,
                w,
                h.min(wa.bottom - wa.top),
                null_mut(),
                null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            return Err("설정 창 만들기 실패".into());
        }
        unsafe {
            let on: u32 = dark as u32;
            DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE as u32, &on as *const u32 as *const _, 4);
        }
        let mut ctx = WebContext::new(Some(usagi_core::config::config_dir().join("webview")));
        let bg = if dark { (28, 28, 28, 255) } else { (243, 243, 243, 255) };
        let host_raw = host as isize;
        let webview = WebViewBuilder::new_with_web_context(&mut ctx)
            // 화면은 모든 플랫폼 공용 (usagi-core). 이 화면만 띄우고, 다른 주소로 이동하거나 새 창을 여는 건 막음
            // (바깥 페이지가 이 창에 뜨면 앱에 명령을 보낼 수 있게 되므로)
            .with_html(usagi_core::ui::settings_html())
            .with_navigation_handler(|url| usagi_core::ui::navigation_allowed(&url))
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_background_color(bg)
            .with_bounds(client_rect(hwnd))
            .with_ipc_handler(move |req| {
                inbox.borrow_mut().push_back(req.body().clone());
                unsafe { PostMessageW(host_raw as HWND, WM_IPC, 0, 0) };
            })
            .build_as_child(&Parent(hwnd));
        let webview = match webview {
            Ok(v) => v,
            Err(e) => {
                unsafe { DestroyWindow(hwnd) };
                return Err(format!("설정 화면(WebView2) 열기 실패: {e}"));
            }
        };
        unsafe {
            ShowWindow(hwnd, SW_SHOW);
            SetForegroundWindow(hwnd);
        }
        Ok(Self { hwnd, webview: Some(webview), _ctx: ctx })
    }

    pub fn resize(&self) {
        if let Some(v) = &self.webview {
            let _ = v.set_bounds(client_rect(self.hwnd));
        }
    }

    pub fn push(&self, state_json: &str) {
        if let Some(v) = &self.webview {
            let _ = v.evaluate_script(&format!("window.usagi && window.usagi.render({state_json})"));
        }
    }

    pub fn focus(&self) {
        unsafe {
            if IsIconic(self.hwnd) != 0 {
                ShowWindow(self.hwnd, SW_RESTORE);
            }
            SetForegroundWindow(self.hwnd);
        }
    }
}

impl Drop for SettingsWindow {
    fn drop(&mut self) {
        self.webview.take(); // 웹뷰 먼저 해제
        unsafe {
            if IsWindow(self.hwnd) != 0 {
                DestroyWindow(self.hwnd);
            }
        }
    }
}
