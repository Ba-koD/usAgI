//! Windows 인터넷 옵션의 프록시 (브라우저·WinHTTP와 같은 설정).
//!
//! 자동 검색(WPAD)이나 PAC 주소가 켜져 있으면 WinHTTP에 이 주소의 프록시를 물어보고(`WinHttpGetProxyForUrl`),
//! 안 되면 수동 프록시와 예외 목록을 씁니다. 실제 연결은 ureq가 해요.

use super::{bypassed, parse_proxy_list, Route};
use std::ptr::{null, null_mut};
use std::sync::OnceLock;
use windows_sys::Win32::Foundation::GlobalFree;
use windows_sys::Win32::Networking::WinHttp::*;

/// 인터넷 옵션의 프록시 설정
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct IeConfig {
    /// "설정 자동 검색" (WPAD)
    pub auto_detect: bool,
    /// "자동 구성 스크립트 사용" 주소 (PAC)
    pub pac_url: Option<String>,
    /// 수동 프록시 (`host:port` 또는 `https=host:port;...`)
    pub proxy: Option<String>,
    /// 예외 목록 (`<local>;*.corp`)
    pub bypass: Option<String>,
}

pub(super) fn system_routes(url: &str, scheme: &str, host: &str) -> Vec<Route> {
    ie_config().map(|config| resolve(url, scheme, host, &config)).unwrap_or_default()
}

fn ie_config() -> Option<IeConfig> {
    let mut raw = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG {
        fAutoDetect: 0,
        lpszAutoConfigUrl: null_mut(),
        lpszProxy: null_mut(),
        lpszProxyBypass: null_mut(),
    };
    if unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut raw) } == 0 {
        return None;
    }
    unsafe {
        Some(IeConfig {
            auto_detect: raw.fAutoDetect != 0,
            pac_url: take(raw.lpszAutoConfigUrl),
            proxy: take(raw.lpszProxy),
            bypass: take(raw.lpszProxyBypass),
        })
    }
}

/// 설정으로 이 주소의 경로를 정함 (빈 목록 = 직접)
pub(super) fn resolve(url: &str, scheme: &str, host: &str, config: &IeConfig) -> Vec<Route> {
    if config.auto_detect || config.pac_url.is_some() {
        if let Some(routes) = auto_proxy(url, scheme, config.auto_detect, config.pac_url.as_deref()) {
            return routes;
        }
    }
    match &config.proxy {
        Some(list) if !config.bypass.as_deref().is_some_and(|b| bypassed(host, b)) => parse_proxy_list(list, scheme),
        _ => Vec::new(),
    }
}

/// 자동 검색·PAC로 프록시 묻기. 실패하면(검색 실패, PAC 오류) None → 수동 설정으로
fn auto_proxy(url: &str, scheme: &str, detect: bool, pac_url: Option<&str>) -> Option<Vec<Route>> {
    let session = session()?;
    let url = wide(url);
    let pac = pac_url.map(wide);
    let mut options = WINHTTP_AUTOPROXY_OPTIONS {
        dwFlags: 0,
        dwAutoDetectFlags: 0,
        lpszAutoConfigUrl: null(),
        lpvReserved: null_mut(),
        dwReserved: 0,
        fAutoLogonIfChallenged: 0,
    };
    if detect {
        options.dwFlags |= WINHTTP_AUTOPROXY_AUTO_DETECT;
        options.dwAutoDetectFlags = WINHTTP_AUTO_DETECT_TYPE_DHCP | WINHTTP_AUTO_DETECT_TYPE_DNS_A;
    }
    if let Some(pac) = &pac {
        options.dwFlags |= WINHTTP_AUTOPROXY_CONFIG_URL;
        options.lpszAutoConfigUrl = pac.as_ptr();
    }
    let mut info = WINHTTP_PROXY_INFO { dwAccessType: 0, lpszProxy: null_mut(), lpszProxyBypass: null_mut() };
    // PAC 서버가 로그인을 요구해도 Windows 자격 증명으로 다시 시도하지 않음
    // (자동 검색으로 찾은 PAC는 네트워크의 누구나 낼 수 있어, 자격 응답을 가로채 쓸 수 있음) → 수동 설정으로
    if unsafe { WinHttpGetProxyForUrl(session as _, url.as_ptr(), &mut options, &mut info) } == 0 {
        return None;
    }
    let (proxy, _bypass) = unsafe { (take(info.lpszProxy), take(info.lpszProxyBypass)) };
    Some(match proxy {
        Some(list) if info.dwAccessType == WINHTTP_ACCESS_TYPE_NAMED_PROXY => parse_proxy_list(&list, scheme),
        _ => vec![Route::Direct],
    })
}

/// 프록시를 묻는 데만 쓰는 WinHTTP 세션 (자동 검색 결과를 WinHTTP가 기억하도록 하나를 계속 씀)
fn session() -> Option<usize> {
    static SESSION: OnceLock<usize> = OnceLock::new();
    let handle = *SESSION.get_or_init(|| unsafe {
        let agent = wide(concat!("usAgI/", env!("CARGO_PKG_VERSION")));
        let handle = WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_NO_PROXY, null(), null(), 0);
        if !handle.is_null() {
            // PAC 파일 받기가 오래 걸리지 않게
            WinHttpSetTimeouts(handle, 5_000, 5_000, 5_000, 5_000);
        }
        handle as usize
    });
    (handle != 0).then_some(handle)
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// WinHTTP가 준 문자열을 옮기고 메모리 반환. 비었으면 None
unsafe fn take(text: windows_sys::core::PWSTR) -> Option<String> {
    if text.is_null() {
        return None;
    }
    let mut len = 0;
    while *text.add(len) != 0 {
        len += 1;
    }
    let value = String::from_utf16_lossy(std::slice::from_raw_parts(text, len));
    GlobalFree(text as _);
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    /// PAC 파일을 계속 주는 서버
    fn pac_server(script: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/proxy.pac", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                crate::net::tests::read_head(&mut stream);
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/x-ns-proxy-autoconfig\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{script}",
                    script.len()
                );
            }
        });
        url
    }

    #[test]
    fn manual_proxy_and_bypass_list() {
        let config = IeConfig { proxy: Some("http=web:80;https=secure:8443".into()), bypass: Some("<local>;*.corp".into()), ..Default::default() };
        let url = "https://api.github.com/x";
        assert_eq!(resolve(url, "https", "api.github.com", &config), vec![Route::Proxy("http://secure:8443".into())]);
        assert_eq!(resolve("https://git.corp/", "https", "git.corp", &config), vec![]);
        assert_eq!(resolve("https://intranet/", "https", "intranet", &config), vec![]);
    }

    #[test]
    fn pac_script_decides_the_route() {
        let pac = pac_server(
            "function FindProxyForURL(url, host) { if (host == 'api.github.com') return 'PROXY 127.0.0.1:3128; DIRECT'; return 'DIRECT'; }",
        );
        let config = IeConfig { pac_url: Some(pac), proxy: Some("manual:1".into()), ..Default::default() };
        let routes = resolve("https://api.github.com/x", "https", "api.github.com", &config);
        assert_eq!(routes.first(), Some(&Route::Proxy("http://127.0.0.1:3128".into())), "{routes:?}");
        assert_eq!(resolve("https://example.com/", "https", "example.com", &config), vec![Route::Direct], "PAC가 DIRECT면 수동 설정보다 우선");
    }

    #[test]
    fn broken_pac_falls_back_to_the_manual_proxy() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead = format!("http://{}/none.pac", listener.local_addr().unwrap());
        drop(listener);
        let config = IeConfig { pac_url: Some(dead), proxy: Some("manual:3128".into()), ..Default::default() };
        assert_eq!(resolve("https://api.github.com/", "https", "api.github.com", &config), vec![Route::Proxy("http://manual:3128".into())]);
    }
}
