//! macOS 네트워크 설정의 프록시 (Safari·시스템과 같은 설정).
//!
//! 시스템 설정을 CFNetwork에 넘겨 이 주소의 프록시 목록을 받고(예외 목록 반영), PAC 주소·스크립트면 CFNetwork로 실행해요.
//! PAC는 지금 스레드의 런 루프에서 전용 모드로 돌리고 정해진 시간 안에 끝나지 않으면 직접 연결로 봅니다.

use super::Route;
use objc2_cf_network::*;
use objc2_core_foundation::*;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

/// PAC 실행(파일 받기 포함)을 기다리는 최대 시간
const PAC_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) fn system_routes(url: &str) -> Vec<Route> {
    match unsafe { CFNetworkCopySystemProxySettings() } {
        Some(settings) => routes_with(url, &settings),
        None => Vec::new(),
    }
}

/// 프록시 설정(시스템 설정과 같은 형식의 사전)으로 이 주소의 경로를 정함 (빈 목록 = 직접)
pub(super) fn routes_with(url: &str, settings: &CFDictionary) -> Vec<Route> {
    let Some(target) = CFURL::from_string(None, &CFString::from_str(url), None) else { return Vec::new() };
    let list = unsafe { CFNetworkCopyProxiesForURL(&target, settings) };
    let mut routes = Vec::new();
    for entry in dictionaries(&list) {
        // PAC 항목 뒤의 항목들(보통 "직접")은 PAC가 실패했을 때의 대안이라, PAC가 답하면 거기서 끝
        if let Some(pac) = pac_entry(entry) {
            match pac_routes(pac, &target) {
                Some(answer) => {
                    routes.extend(answer);
                    break;
                }
                None => continue,
            }
        }
        routes.extend(entry_routes(entry));
    }
    routes
}

/// PAC 주소·스크립트 항목이면 그 PAC
fn pac_entry(entry: &CFDictionary) -> Option<Pac<'_>> {
    let kind = string(entry, unsafe { kCFProxyTypeKey })?;
    if kind == unsafe { kCFProxyTypeAutoConfigurationURL }.to_string() {
        value(entry, unsafe { kCFProxyAutoConfigurationURLKey })?.downcast_ref::<CFURL>().map(Pac::Url)
    } else if kind == unsafe { kCFProxyTypeAutoConfigurationJavaScript }.to_string() {
        value(entry, unsafe { kCFProxyAutoConfigurationJavaScriptKey })?.downcast_ref::<CFString>().map(Pac::Script)
    } else {
        None
    }
}

/// 프록시 목록의 항목 하나 → 경로 (PAC·SOCKS·FTP 항목은 빈 목록)
fn entry_routes(entry: &CFDictionary) -> Vec<Route> {
    let Some(kind) = string(entry, unsafe { kCFProxyTypeKey }) else { return Vec::new() };
    let is = |constant: &CFString| kind == constant.to_string();
    if is(unsafe { kCFProxyTypeNone }) {
        vec![Route::Direct]
    } else if is(unsafe { kCFProxyTypeHTTP }) || is(unsafe { kCFProxyTypeHTTPS }) {
        let host = string(entry, unsafe { kCFProxyHostNameKey });
        let port = number(entry, unsafe { kCFProxyPortNumberKey }).unwrap_or(80);
        // IPv6 주소는 대괄호로 감싸야 주소로 읽힘
        host.filter(|h| !h.is_empty())
            .map(|h| if h.contains(':') && !h.starts_with('[') { format!("[{h}]") } else { h })
            .map(|h| vec![Route::Proxy(format!("http://{h}:{port}"))])
            .unwrap_or_default()
    } else {
        Vec::new()
    }
}

enum Pac<'a> {
    Url(&'a CFURL),
    Script(&'a CFString),
}

/// PAC 실행 결과 (콜백이 채움)
struct PacResult {
    done: bool,
    list: Option<CFRetained<CFArray>>,
}

unsafe extern "C-unwind" fn pac_done(client: NonNull<c_void>, list: NonNull<CFArray>, error: *mut CFError) {
    let result = client.cast::<PacResult>().as_ptr();
    (*result).done = true;
    if error.is_null() {
        (*result).list = Some(CFRetained::retain(list));
    }
}

/// PAC를 실행해 경로를 받음. 실패하거나 시간 안에 끝나지 않으면 None (설정의 다음 항목으로)
fn pac_routes(pac: Pac, target: &CFURL) -> Option<Vec<Route>> {
    let run_loop = CFRunLoop::current()?;
    let result = Box::into_raw(Box::new(PacResult { done: false, list: None }));
    let mut context = CFStreamClientContext { version: 0, info: result.cast(), retain: None, release: None, copyDescription: None };
    let context_ptr = NonNull::from(&mut context);
    let source = unsafe {
        match pac {
            Pac::Url(url) => CFNetworkExecuteProxyAutoConfigurationURL(url, target, Some(pac_done), context_ptr),
            Pac::Script(script) => CFNetworkExecuteProxyAutoConfigurationScript(script, target, Some(pac_done), context_ptr),
        }
    };
    let mode = CFString::from_static_str("dev.usagi.proxy-pac");
    run_loop.add_source(Some(&source), Some(&mode));
    let deadline = Instant::now() + PAC_TIMEOUT;
    while !unsafe { (*result).done } && Instant::now() < deadline {
        CFRunLoop::run_in_mode(Some(&mode), 0.25, true);
    }
    run_loop.remove_source(Some(&source), Some(&mode));
    // 끝나지 않았어도 이후에 콜백이 오지 않게
    source.invalidate();
    let result = unsafe { Box::from_raw(result) };
    let routes: Vec<Route> = dictionaries(result.list.as_deref()?).into_iter().flat_map(entry_routes).collect();
    // 지원하지 않는 경로(SOCKS)만 있으면 직접
    Some(if routes.is_empty() { vec![Route::Direct] } else { routes })
}

fn dictionaries(list: &CFArray) -> Vec<&CFDictionary> {
    (0..list.count())
        .filter_map(|i| unsafe { list.value_at_index(i).cast::<CFType>().as_ref() })
        .filter_map(|item| item.downcast_ref::<CFDictionary>())
        .collect()
}

fn value<'a>(dict: &'a CFDictionary, key: &CFString) -> Option<&'a CFType> {
    unsafe { dict.value((key as *const CFString).cast()).cast::<CFType>().as_ref() }
}

fn string(dict: &CFDictionary, key: &CFString) -> Option<String> {
    value(dict, key)?.downcast_ref::<CFString>().map(|s| s.to_string())
}

fn number(dict: &CFDictionary, key: &CFString) -> Option<i64> {
    value(dict, key)?.downcast_ref::<CFNumber>()?.as_i64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::net::TcpListener;

    /// 시스템 설정과 같은 형식의 프록시 설정 사전
    fn settings(pairs: &[(&CFString, &CFType)]) -> CFRetained<CFDictionary> {
        let keys: Vec<&CFString> = pairs.iter().map(|(k, _)| *k).collect();
        let values: Vec<&CFType> = pairs.iter().map(|(_, v)| *v).collect();
        let dict = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
        unsafe { CFRetained::cast_unchecked(dict) }
    }

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
    fn manual_https_proxy_and_exceptions() {
        let one = CFNumber::new_i32(1);
        let port = CFNumber::new_i32(3128);
        let host = CFString::from_str("proxy.corp");
        let exception = CFString::from_str("*.corp");
        let exceptions = CFArray::from_objects(&[&*exception]);
        let dict = settings(&[
            (unsafe { kCFNetworkProxiesHTTPSEnable }, &one),
            (unsafe { kCFNetworkProxiesHTTPSProxy }, &host),
            (unsafe { kCFNetworkProxiesHTTPSPort }, &port),
            (unsafe { kCFNetworkProxiesExceptionsList }, &exceptions),
        ]);
        assert_eq!(routes_with("https://api.github.com/x", &dict), vec![Route::Proxy("http://proxy.corp:3128".into())]);
        assert_eq!(routes_with("https://git.corp/x", &dict), vec![Route::Direct], "예외 목록은 직접");
        assert_eq!(routes_with("https://api.github.com/x", &settings(&[])), vec![Route::Direct], "설정이 없으면 직접");
    }

    #[test]
    fn pac_url_decides_the_route() {
        let pac = pac_server(
            "function FindProxyForURL(url, host) { if (host == 'api.github.com') return 'PROXY 127.0.0.1:3128; DIRECT'; return 'DIRECT'; }",
        );
        let one = CFNumber::new_i32(1);
        let pac = CFString::from_str(&pac);
        let dict = settings(&[
            (unsafe { kCFNetworkProxiesProxyAutoConfigEnable }, &one),
            (unsafe { kCFNetworkProxiesProxyAutoConfigURLString }, &pac),
        ]);
        assert_eq!(
            routes_with("https://api.github.com/x", &dict),
            vec![Route::Proxy("http://127.0.0.1:3128".into()), Route::Direct]
        );
        assert_eq!(routes_with("https://example.com/", &dict), vec![Route::Direct]);
    }

    #[test]
    fn pac_script_and_broken_pac() {
        let one = CFNumber::new_i32(1);
        let script = CFString::from_str("function FindProxyForURL(url, host) { return 'PROXY a.example:8080; PROXY b.example:8081'; }");
        let dict = settings(&[
            (unsafe { kCFNetworkProxiesProxyAutoConfigEnable }, &one),
            (unsafe { kCFNetworkProxiesProxyAutoConfigJavaScript }, &script),
        ]);
        assert_eq!(
            routes_with("https://api.github.com/", &dict),
            vec![Route::Proxy("http://a.example:8080".into()), Route::Proxy("http://b.example:8081".into())]
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead = CFString::from_str(&format!("http://{}/none.pac", listener.local_addr().unwrap()));
        drop(listener);
        let dict = settings(&[
            (unsafe { kCFNetworkProxiesProxyAutoConfigEnable }, &one),
            (unsafe { kCFNetworkProxiesProxyAutoConfigURLString }, &dead),
        ]);
        let started = Instant::now();
        assert_eq!(routes_with("https://api.github.com/", &dict), vec![Route::Direct], "PAC를 못 받으면 직접");
        assert!(started.elapsed() < PAC_TIMEOUT + Duration::from_secs(1));
    }
}
