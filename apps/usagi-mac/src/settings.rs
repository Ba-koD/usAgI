//! macOS 설정 창: 모든 플랫폼 공용 설정 화면(usagi-core)을 WKWebView로 띄움.
//! 화면 → 앱: window.ipc.postMessage(JSON) → UserEvent::Ipc
//! 앱 → 화면: window.usagi.render(state)

use crate::app::UserEvent;
use tao::dpi::LogicalSize;
use tao::event_loop::{EventLoopProxy, EventLoopWindowTarget};
use tao::window::{Window, WindowBuilder, WindowId};
use wry::{WebView, WebViewBuilder};

pub struct SettingsWindow {
    // 웹뷰를 창보다 먼저 정리
    webview: WebView,
    window: Window,
}

impl SettingsWindow {
    pub fn open(target: &EventLoopWindowTarget<UserEvent>, proxy: EventLoopProxy<UserEvent>) -> Result<Self, String> {
        let window = WindowBuilder::new()
            .with_title("usAgI 설정")
            .with_inner_size(LogicalSize::new(980.0, 660.0))
            .with_min_inner_size(LogicalSize::new(480.0, 440.0))
            .build(target)
            .map_err(|e| format!("설정 창 만들기 실패: {e}"))?;
        let webview = WebViewBuilder::new()
            // 이 화면만 띄우고, 다른 주소로 이동하거나 새 창을 여는 건 막음
            // (바깥 페이지가 이 창에 뜨면 앱에 명령을 보낼 수 있게 되므로)
            .with_html(usagi_core::ui::settings_html())
            .with_navigation_handler(|url| {
                let ok = usagi_core::ui::navigation_allowed(&url);
                if !ok {
                    crate::app::log("설정 화면의 다른 주소 이동 차단");
                }
                ok
            })
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_ipc_handler(move |req| {
                let _ = proxy.send_event(UserEvent::Ipc(req.body().clone()));
            })
            .build(&window)
            .map_err(|e| format!("설정 화면 열기 실패: {e}"))?;
        window.set_focus();
        Ok(Self { webview, window })
    }

    pub fn id(&self) -> WindowId {
        self.window.id()
    }

    pub fn focus(&self) {
        self.window.set_visible(true);
        self.window.set_focus();
    }

    pub fn push(&self, state_json: &str) {
        let _ = self.webview.evaluate_script(&format!("window.usagi && window.usagi.render({state_json})"));
    }
}
