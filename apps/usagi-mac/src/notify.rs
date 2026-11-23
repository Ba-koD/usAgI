//! macOS 알림.
//!
//! .app 묶음으로 실행 중이면 앱 자체 알림: 알림 센터에 usAgI 이름·아이콘으로 표시되고,
//! 시스템 설정의 알림·집중 모드 목록에도 usAgI로 나와서 방해 금지 중 허용 앱으로 추가 가능.
//! 알림이 꺼져 있으면 시스템이 표시하지 않음 (다른 경로로 우회하지 않음).
//! 묶음 밖(개발 실행)에서만 osascript 알림 (스크립트 편집기 아이콘).

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread};
use objc2_foundation::{NSBundle, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotification, UNNotificationDefaultActionIdentifier,
    UNNotificationPresentationOptions, UNNotificationRequest, UNNotificationResponse, UNNotificationSound, UNUserNotificationCenter,
    UNUserNotificationCenterDelegate,
};
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::Mutex;

/// 0 앱 알림 사용, 1 앱 묶음 밖 실행 → osascript
static FALLBACK: AtomicU8 = AtomicU8::new(1);
static SEQ: AtomicU64 = AtomicU64::new(0);
/// 알림을 눌렀을 때 (설정 창 열기)
static ON_CLICK: Mutex<Option<Box<dyn Fn() + Send>>> = Mutex::new(None);

define_class!(
    // SAFETY: NSObject는 하위 클래스 조건이 없고, Drop 없음
    #[unsafe(super(NSObject))]
    #[name = "UsagiNotificationDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        /// 설정 창이 앞에 있어도 배너로 표시 (기본은 앱이 앞에 있으면 숨김)
        #[unsafe(method(userNotificationCenter:willPresentNotification:withCompletionHandler:))]
        fn will_present(
            &self,
            _center: &UNUserNotificationCenter,
            _notification: &UNNotification,
            handler: &block2::DynBlock<dyn Fn(UNNotificationPresentationOptions)>,
        ) {
            handler.call((UNNotificationPresentationOptions::Banner
                | UNNotificationPresentationOptions::List
                | UNNotificationPresentationOptions::Sound,));
        }

        /// 알림을 누르면 설정 창
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn did_receive(&self, _center: &UNUserNotificationCenter, response: &UNNotificationResponse, handler: &block2::DynBlock<dyn Fn()>) {
            if &*response.actionIdentifier() == unsafe { UNNotificationDefaultActionIdentifier } {
                if let Some(f) = ON_CLICK.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    f();
                }
            }
            handler.call(());
        }
    }
);

impl Delegate {
    fn new() -> Retained<Self> {
        let this = Self::alloc().set_ivars(());
        unsafe { msg_send![super(this), init] }
    }
}

/// .app 묶음 안에서 실행 중인지 (밖에서 알림 센터를 부르면 예외로 종료됨)
fn bundled() -> bool {
    let b = NSBundle::mainBundle();
    b.bundleIdentifier().is_some() && b.bundlePath().to_string().ends_with(".app")
}

fn describe(err: *mut NSError) -> Option<String> {
    unsafe { err.as_ref() }.map(|e| e.localizedDescription().to_string())
}

/// 시작할 때 한 번 (주 스레드). 처음이면 시스템이 알림 허용을 물음
pub fn init(on_click: impl Fn() + Send + 'static) {
    if !bundled() {
        crate::app::log("알림: 앱 묶음 밖 실행 → osascript 알림");
        return;
    }
    *ON_CLICK.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(on_click));
    let center = UNUserNotificationCenter::currentNotificationCenter();
    let delegate = Delegate::new();
    center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    // 알림 센터는 대리자를 약하게 잡음 → 앱이 끝날 때까지 남겨 둠
    std::mem::forget(delegate);
    FALLBACK.store(0, Ordering::SeqCst);
    // 허용 여부는 기록만 함. 꺼져 있어도 osascript로 우회하지 않음 (사용자가 시스템 설정에서 켜면 바로 앱 알림으로 표시)
    let done = RcBlock::new(|granted: Bool, err: *mut NSError| match describe(err) {
        Some(e) => crate::app::log(&format!("알림 권한 확인: {e}")),
        None => crate::app::log(&format!("알림 권한: {}", if granted.as_bool() { "허용" } else { "거부" })),
    });
    center.requestAuthorizationWithOptions_completionHandler(UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound, &done);
}

/// 앱 자체 알림으로 보냄. 쓸 수 없으면 false (osascript로 보내라는 뜻)
pub fn send(title: &str, body: &str) -> bool {
    if FALLBACK.load(Ordering::SeqCst) != 0 {
        return false;
    }
    let content = UNMutableNotificationContent::new();
    content.setTitle(&NSString::from_str(title));
    content.setBody(&NSString::from_str(body));
    content.setSound(Some(&UNNotificationSound::defaultSound()));
    let id = NSString::from_str(&format!("usagi-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
    let request = UNNotificationRequest::requestWithIdentifier_content_trigger(&id, &content, None);
    let done = RcBlock::new(|err: *mut NSError| {
        if let Some(e) = describe(err) {
            crate::app::log(&format!("알림 보내기 실패: {e}"));
        }
    });
    UNUserNotificationCenter::currentNotificationCenter().addNotificationRequest_withCompletionHandler(&request, Some(&done));
    true
}
