//! Windows 알림 (토스트). 트레이 풍선 대신 써서 "중요 알림"으로 보낼 수 있음.
//!
//! - 설치 없이 쓰는 앱이라 사용자 레지스트리(HKCU\Software\Classes\AppUserModelId)에 이름·아이콘을 등록합니다.
//!   이렇게 등록돼야 Windows 설정 > 알림 목록에 usAgI가 나타나고, 우선 순위·중요 알림도 정할 수 있어요.
//! - 중요 알림(scenario="urgent")은 Windows 11(빌드 22546 이상)에서 방해 금지 중에도 보입니다.
//!   사용자가 Windows 설정에서 앱별로 끌 수 있어요.
//! - 알림을 누르면 설정 창을 엽니다. 실패하면 호출한 쪽이 트레이 풍선으로 대신 보냅니다.

use crate::win::{self, wide};
use std::cell::RefCell;
use std::path::Path;
use windows::core::HSTRING;
use windows::Data::Xml::Dom::XmlDocument;
use windows::Foundation::TypedEventHandler;
use windows::UI::Notifications::{ToastNotification, ToastNotificationManager, ToastNotificationPriority};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

pub const AUMID: &str = "usAgI.UsageRabbit";

thread_local! {
    /// 누름 이벤트를 받으려면 알림 객체가 살아 있어야 함 (최근 몇 개만)
    static LIVE: RefCell<Vec<ToastNotification>> = const { RefCell::new(Vec::new()) };
}

/// 앱 이름·아이콘 등록 (시작할 때 한 번). 아이콘은 설정 폴더의 PNG.
pub fn register(icon_png: &Path) {
    let key = format!(r"Software\Classes\AppUserModelId\{AUMID}");
    win::reg_set_string(&key, "DisplayName", "usAgI");
    win::reg_set_string(&key, "IconUri", &icon_png.to_string_lossy());
    // 알림 설정 목록의 아이콘 바탕: 토끼 색과 반대로
    let back = if icon_png.file_name().is_some_and(|n| n == LIGHT_ICON) { "FFF3F3F3" } else { "FF141414" };
    win::reg_set_string(&key, "IconBackgroundColor", back);
    unsafe { SetCurrentProcessExplicitAppUserModelID(wide(AUMID).as_ptr()) };
}

/// Windows 설정에서 usAgI의 "중요 알림"을 허용했는지 (읽기만 함. 허용은 사용자가 Windows 설정에서)
pub fn urgent_allowed() -> Option<bool> {
    win::reg_dword(&format!(r"Software\Microsoft\Windows\CurrentVersion\Notifications\Settings\{AUMID}"), "AllowUrgentNotifications").map(|v| v != 0)
}

/// 중요 알림(urgent)을 지원하는 Windows인지 (Windows 11 빌드 22546 이상)
pub fn urgent_supported() -> bool {
    win::reg_string_lm(r"SOFTWARE\Microsoft\Windows NT\CurrentVersion", "CurrentBuildNumber")
        .and_then(|b| b.trim().parse::<u32>().ok())
        .is_some_and(|b| b >= 22546)
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// 알림 보내기. urgent면 방해 금지 중에도 보이는 중요 알림으로.
pub fn show(title: &str, body: &str, urgent: bool, open_settings: (HWND, u32)) -> windows::core::Result<()> {
    let scenario = if urgent && urgent_supported() { r#" scenario="urgent""# } else { "" };
    let xml = format!(
        r#"<toast{scenario} launch="settings"><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text></binding></visual><audio src="ms-winsoundevent:Notification.Default"/></toast>"#,
        xml_escape(title),
        xml_escape(body)
    );
    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    if urgent {
        // 높은 우선순위: 다른 알림보다 위에, 우선 순위 앱으로 지정돼 있으면 방해 금지 중에도 배너로
        let _ = toast.SetPriority(ToastNotificationPriority::High);
    }
    let (host, msg) = (open_settings.0 as isize, open_settings.1);
    toast.Activated(&TypedEventHandler::new(move |_, _| {
        unsafe { PostMessageW(host as HWND, msg, 0, 0) };
        Ok(())
    }))?;
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(AUMID))?;
    notifier.Show(&toast)?;
    LIVE.with(|l| {
        let mut l = l.borrow_mut();
        l.push(toast);
        if l.len() > 8 {
            l.remove(0);
        }
    });
    Ok(())
}

/// 알림에 쓸 앱 아이콘 PNG (exe 아이콘과 같은 그림)
const LIGHT_ICON: &str = "usagi-logo-light.png";

/// 알림 아이콘을 쓰고 등록. 로고 색 설정이 자동이면 알림 창 색(Windows 모드)에 맞춤: 다크면 흰 토끼, 라이트면 진한 토끼.
/// 색마다 파일을 따로 둬서 Windows가 예전 그림을 계속 쓰지 않게 함.
pub fn refresh_icon() {
    let dir = usagi_core::config::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::remove_file(dir.join("usagi-icon.png")); // 예전 사각형 아이콘
    let light = match usagi_core::config::State::load().pref_str("logo", "auto").as_str() {
        "ink" => true,
        "white" => false,
        _ => win::taskbar_is_light(),
    };
    let path = dir.join(if light { LIGHT_ICON } else { "usagi-logo.png" });
    if write_icon(&path, light) || path.exists() {
        register(&path);
    }
}

pub fn write_icon(path: &Path, light: bool) -> bool {
    use usagi_core::rabbit::{app_icon_colored, ICON_INK, ICON_WHITE};
    app_icon_colored(96, if light { ICON_INK } else { ICON_WHITE })
        .and_then(|pm| pm.encode_png().ok())
        .is_some_and(|bytes| std::fs::write(path, bytes).is_ok())
}

fn shortcut_path() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA").map(|appdata| Path::new(&appdata).join(r"Microsoft\Windows\Start Menu\Programs\usAgI.lnk"))
}

/// 가리키는 exe가 없어진 시작 메뉴 바로 가기를 지움 (그 경로에 나중에 다른 파일이 생기면 시작 메뉴에서 실행되므로)
pub fn remove_stale_shortcut() {
    let Some(lnk) = shortcut_path().filter(|lnk| lnk.is_file()) else { return };
    if shortcut_target(&lnk).is_some_and(|target| !target.is_file()) {
        let _ = std::fs::remove_file(&lnk);
    }
}

/// 바로 가기가 실행하는 파일 (읽을 수 없거나 비어 있으면 None)
fn shortcut_target(lnk: &Path) -> Option<std::path::PathBuf> {
    use windows::core::Interface;
    use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER, STGM_READ};
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).ok()?;
        link.cast::<IPersistFile>().ok()?.Load(&HSTRING::from(lnk.as_os_str()), STGM_READ).ok()?;
        let mut buf = vec![0u16; 32 * 1024];
        link.GetPath(&mut buf, std::ptr::null_mut(), 0).ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        (len > 0).then(|| std::path::PathBuf::from(String::from_utf16_lossy(&buf[..len])))
    }
}

/// 시작 메뉴 바로 가기 (앱 ID 포함). 설치형 데스크톱 앱이 Windows 알림 설정 목록에 나타나는 공식 방법.
/// 이게 있어야 Windows 설정 > 알림에서 usAgI를 우선 순위 앱으로 고르거나 중요 알림을 허용할 수 있음.
/// 내려받기·임시 폴더의 exe로는 만들지 않음 (호출하는 쪽이 확인)
pub fn ensure_shortcut(exe: &Path) -> windows::core::Result<()> {
    use windows::core::Interface;
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
    use windows::Win32::System::Com::{CoCreateInstance, IPersistFile, CLSCTX_INPROC_SERVER};
    use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    let Some(lnk) = shortcut_path() else { return Ok(()) };
    if let Some(dir) = lnk.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(exe.as_os_str()))?;
        if let Some(d) = exe.parent() {
            link.SetWorkingDirectory(&HSTRING::from(d.as_os_str()))?;
        }
        link.SetDescription(&HSTRING::from(usagi_core::tr!("usAgI — AI 구독 사용량 표시", "usAgI — AI subscription usage")))?;
        let store: IPropertyStore = link.cast()?;
        store.SetValue(&PKEY_AppUserModel_ID, &PROPVARIANT::from(AUMID))?;
        store.Commit()?;
        let file: IPersistFile = link.cast()?;
        file.Save(&HSTRING::from(lnk.as_os_str()), true)?;
    }
    Ok(())
}
