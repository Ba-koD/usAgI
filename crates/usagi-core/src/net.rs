//! HTTPS 연결 공통: TLS 설정과 OS 프록시.
//!
//! 프록시는 브라우저처럼 OS 설정을 따릅니다.
//! 1. 루프백 주소(localhost, 127.x, ::1)는 늘 직접 연결
//! 2. 환경 변수 `HTTPS_PROXY`·`ALL_PROXY`·`HTTP_PROXY`가 있으면 그것 (`NO_PROXY` 포함)
//! 3. OS 설정: Windows는 인터넷 옵션(수동 프록시·PAC 주소·자동 검색), macOS는 네트워크 설정(수동 프록시·PAC·자동 검색)
//! 4. 아무것도 없으면 직접 연결
//!
//! PAC가 경로를 여러 개 주면(`PROXY a; PROXY b; DIRECT`) 연결이 안 될 때 다음 경로로 넘어가요 (앞에서 최대 4개).
//! OS·PAC에는 `스킴://호스트/`만 넘겨요 (주소의 경로·쿼리에 든 조직 ID나 서명된 다운로드 주소를 PAC에 보이지 않게).
//! OS 설정 조회는 주소(스킴+호스트)마다 잠깐 기억해 두고, SOCKS 프록시는 지원하지 않아 건너뜁니다.
//!
//! `http://` 프록시만 씁니다. `https://` 프록시를 거치면 ureq가 목적지와의 TLS를 생략해 토큰이 프록시에 평문으로 가므로,
//! 그런 설정만 있으면 직접 연결로 우회하지 않고 연결하지 않아요 (fail closed).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

/// OS 설정을 다시 읽기 전까지 기억하는 시간 (자동 검색이 느린 PC에서 매번 기다리지 않게)
const CACHE_TTL: Duration = Duration::from_secs(10 * 60);
/// 요청 하나가 시도하는 경로 수 (PAC가 경로를 아주 많이 줘도 경로마다 시간 제한을 기다리며 멈추지 않게)
const MAX_ROUTES: usize = 4;

/// 요청 하나가 거칠 경로 (OS 설정을 읽는 Windows·macOS가 만듦)
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(not(any(windows, target_os = "macos", test)), allow(dead_code))]
pub(crate) enum Route {
    Direct,
    /// `http://host:port` 형식의 프록시 (HTTPS는 CONNECT로 통과)
    Proxy(String),
}

/// Windows·macOS·Linux 공통 TLS 설정: 이 OS의 인증서 저장소를 믿음 (회사 루트 인증서 포함)
pub(crate) fn tls(config: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>) -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
    #[cfg(any(windows, target_os = "macos"))]
    let config = config.tls_config(
        ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build(),
    );
    config
}

/// 경로를 차례로 시도해 응답을 받음. 응답이 오면(상태 코드와 상관없이) 그것을, 연결이 안 되면 다음 경로를 시도.
/// `send`는 받은 프록시(`None`이면 직접)로 요청을 한 번 보내요.
pub(crate) fn call<T>(url: &str, send: impl FnMut(Option<ureq::Proxy>) -> Result<T, ureq::Error>) -> Result<T, ureq::Error> {
    call_with(proxies_for(url), send)
}

fn call_with<T>(
    proxies: Vec<Option<ureq::Proxy>>,
    mut send: impl FnMut(Option<ureq::Proxy>) -> Result<T, ureq::Error>,
) -> Result<T, ureq::Error> {
    let mut last = ureq::Error::ConnectionFailed;
    for proxy in proxies {
        match send(proxy) {
            Ok(response) => return Ok(response),
            Err(error) => last = error,
        }
    }
    Err(last)
}

/// 이 주소로 갈 때 시도할 프록시 목록 (`None`은 직접 연결). 비었으면 쓸 수 있는 경로가 없음 (연결하지 않음).
pub(crate) fn proxies_for(url: &str) -> Vec<Option<ureq::Proxy>> {
    let Some((scheme, host)) = scheme_host(url) else { return vec![None] };
    if is_loopback(&host) {
        return vec![None];
    }
    if let Some(proxy) = ureq::Proxy::try_from_env() {
        return if proxy.protocol() == ureq::ProxyProtocol::Http { vec![Some(proxy)] } else { Vec::new() };
    }
    to_proxies(&system_routes_cached(&scheme, &host))
}

/// 경로 목록 → 시도할 프록시. 설정이 없으면 직접, 설정은 있는데 쓸 수 있는 게 없으면 빈 목록 (직접으로 우회하지 않음)
fn to_proxies(routes: &[Route]) -> Vec<Option<ureq::Proxy>> {
    if routes.is_empty() {
        return vec![None];
    }
    let mut list = Vec::new();
    for route in routes {
        let proxy = match route {
            Route::Direct => None,
            Route::Proxy(url) if url.starts_with("http://") => match ureq::Proxy::new(url) {
                Ok(proxy) if proxy.protocol() == ureq::ProxyProtocol::Http => Some(proxy),
                _ => continue,
            },
            Route::Proxy(_) => continue,
        };
        if !list.contains(&proxy) {
            list.push(proxy);
        }
        if list.len() == MAX_ROUTES {
            break;
        }
    }
    list
}

/// 주소(스킴+호스트) → (읽은 시각, 경로)
type RouteCache = HashMap<String, (Instant, Vec<Route>)>;

static CACHE: Mutex<Option<RouteCache>> = Mutex::new(None);

fn system_routes_cached(scheme: &str, host: &str) -> Vec<Route> {
    let key = format!("{scheme}://{host}");
    let now = Instant::now();
    if let Some((at, routes)) = CACHE.lock().ok().and_then(|cache| cache.as_ref()?.get(&key).cloned()) {
        if now.duration_since(at) < CACHE_TTL {
            return routes;
        }
    }
    // PAC에는 호스트까지만 (경로·쿼리는 보이지 않게). IPv6 주소는 대괄호로
    let origin = if host.contains(':') { format!("{scheme}://[{host}]/") } else { format!("{scheme}://{host}/") };
    let routes = system_routes(&origin, scheme, host);
    if let Ok(mut cache) = CACHE.lock() {
        let map = cache.get_or_insert_with(HashMap::new);
        map.retain(|_, (at, _)| now.duration_since(*at) < CACHE_TTL);
        map.insert(key, (now, routes.clone()));
    }
    routes
}

#[allow(unused_variables)]
fn system_routes(url: &str, scheme: &str, host: &str) -> Vec<Route> {
    #[cfg(windows)]
    return windows::system_routes(url, scheme, host);
    #[cfg(target_os = "macos")]
    return macos::system_routes(url);
    #[cfg(not(any(windows, target_os = "macos")))]
    Vec::new()
}

/// OS 설정을 다시 읽게 기억을 비움 (테스트에서 프록시 설정을 바꾼 뒤)
#[cfg(test)]
pub(crate) fn clear_cache() {
    if let Ok(mut cache) = CACHE.lock() {
        *cache = None;
    }
}

/// `https://Host:443/path` → ("https", "host")
pub(crate) fn scheme_host(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split_once(']')?.0
    } else {
        authority.split(':').next()?
    };
    (!host.is_empty()).then(|| (scheme.to_ascii_lowercase(), host.to_ascii_lowercase()))
}

fn is_loopback(host: &str) -> bool {
    host == "localhost"
        || host.ends_with(".localhost")
        || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Windows 프록시 목록 중 이 스킴에 쓸 것. 형식: `host:port`, `https=host:port;http=host:port`, 여러 개는 `;`나 공백.
/// 이 스킴 전용 항목이 있으면 그것만, 없으면 스킴 없는 항목. `socks=`·`socks://`는 건너뜀.
#[cfg(any(windows, test))]
pub(crate) fn parse_proxy_list(list: &str, scheme: &str) -> Vec<Route> {
    let entries: Vec<(Option<String>, &str)> = list
        .split(|c: char| c == ';' || c.is_whitespace())
        .filter(|e| !e.is_empty())
        .map(|e| match e.split_once('=') {
            Some((s, value)) => (Some(s.trim().to_ascii_lowercase()), value.trim()),
            None => (None, e),
        })
        .collect();
    let specific = entries.iter().any(|(s, _)| s.as_deref() == Some(scheme));
    entries
        .iter()
        .filter(|(s, _)| if specific { s.as_deref() == Some(scheme) } else { s.is_none() })
        .filter_map(|(_, value)| proxy_route(value))
        .collect()
}

#[cfg(any(windows, test))]
fn proxy_route(value: &str) -> Option<Route> {
    if value.eq_ignore_ascii_case("DIRECT") {
        return Some(Route::Direct);
    }
    let (scheme, address) = match value.split_once("://") {
        Some((s, a)) => (s.to_ascii_lowercase(), a),
        None => ("http".to_string(), value),
    };
    let address = address.trim_end_matches('/');
    // https:// 프록시는 목적지 TLS가 생략되므로 쓰지 않음 (모듈 설명 참고)
    (scheme == "http" && !address.is_empty()).then(|| Route::Proxy(format!("http://{address}")))
}

/// Windows 예외 목록(`<local>;*.corp.example;10.*`)에 이 호스트가 있는지
#[cfg(any(windows, test))]
pub(crate) fn bypassed(host: &str, list: &str) -> bool {
    let host = host.to_ascii_lowercase();
    list.split(|c: char| c == ';' || c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .any(|entry| {
            if entry.eq_ignore_ascii_case("<local>") {
                return !host.contains('.') && !host.contains(':');
            }
            let pattern = entry.split_once("://").map_or(entry, |(_, rest)| rest).to_ascii_lowercase();
            let pattern = match pattern.rsplit_once(':') {
                Some((name, port)) if !name.contains(':') && port.chars().all(|c| c.is_ascii_digit()) => name.to_string(),
                _ => pattern,
            };
            glob(&pattern, &host)
        })
}

/// `*`만 쓰는 와일드카드 비교
#[cfg(any(windows, test))]
fn glob(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || !text[first.len()..].ends_with(last) || text.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for middle in &parts[1..parts.len() - 1] {
        match rest.find(middle) {
            Some(at) => rest = &rest[at + middle.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn proxy(url: &str) -> Route {
        Route::Proxy(url.into())
    }

    #[test]
    fn proxy_list_picks_the_entries_for_the_scheme() {
        assert_eq!(parse_proxy_list("proxy.corp:8080", "https"), vec![proxy("http://proxy.corp:8080")]);
        assert_eq!(
            parse_proxy_list("http=web:80;https=secure:443", "https"),
            vec![proxy("http://secure:443")]
        );
        assert_eq!(parse_proxy_list("http=web:80", "https"), vec![], "HTTPS 전용이 없고 공통도 없으면 직접");
        assert_eq!(
            parse_proxy_list("a:1; b:2", "https"),
            vec![proxy("http://a:1"), proxy("http://b:2")],
            "PAC 결과는 순서대로"
        );
        assert_eq!(parse_proxy_list("socks=s:1080", "https"), vec![]);
        assert_eq!(parse_proxy_list("socks5://s:1080", "https"), vec![]);
        assert_eq!(parse_proxy_list("HTTPS=https://tls-proxy:443/", "https"), vec![], "https:// 프록시는 쓰지 않음");
        assert_eq!(parse_proxy_list("HTTPS=http://plain-proxy:8080/", "https"), vec![proxy("http://plain-proxy:8080")]);
        assert_eq!(parse_proxy_list("a:1;DIRECT", "https"), vec![proxy("http://a:1"), Route::Direct]);
    }

    #[test]
    fn bypass_list_matches_windows_patterns() {
        let list = "<local>;*.corp.example;10.*;api.internal:8443";
        assert!(bypassed("intranet", list));
        assert!(bypassed("build.corp.example", list));
        assert!(!bypassed("corp.example", list), "*.x는 x 자체와 다름");
        assert!(bypassed("10.1.2.3", list));
        assert!(bypassed("API.internal", list));
        assert!(!bypassed("api.github.com", list));
        assert!(!bypassed("api.github.com", ""));
        assert!(glob("*github*", "api.github.com"));
        assert!(!glob("a*b*c", "acb"));
    }

    #[test]
    fn hosts_are_read_from_urls() {
        assert_eq!(scheme_host("https://API.GitHub.com/repos/x"), Some(("https".into(), "api.github.com".into())));
        assert_eq!(scheme_host("http://user:pw@127.0.0.1:8080"), Some(("http".into(), "127.0.0.1".into())));
        assert_eq!(scheme_host("https://[::1]:443/"), Some(("https".into(), "::1".into())));
        assert_eq!(scheme_host("not a url"), None);
        assert!(is_loopback("localhost") && is_loopback("127.0.0.9") && is_loopback("::1"));
        assert!(!is_loopback("api.github.com"));
        assert_eq!(proxies_for("http://127.0.0.1:1/x"), vec![None], "루프백은 늘 직접");
    }

    #[test]
    fn routes_become_unique_proxies_with_direct_fallback() {
        let routes = [proxy("http://a:1"), proxy("http://a:1"), Route::Direct, proxy("socks5://bad")];
        let list = to_proxies(&routes);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].as_ref().map(|p| (p.host().to_string(), p.port())), Some(("a".into(), 1)));
        assert!(list[1].is_none());
        assert_eq!(to_proxies(&[]), vec![None], "설정이 없으면 직접");
        assert!(to_proxies(&[proxy("https://tls-proxy:443")]).is_empty(), "https:// 프록시만 있으면 직접으로 우회하지 않음");
        assert!(to_proxies(&[proxy("socks5://s:1080")]).is_empty());
        let many: Vec<Route> = (0..2000).map(|i| proxy(&format!("http://p{i}.example:8080"))).collect();
        assert_eq!(to_proxies(&many).len(), MAX_ROUTES, "PAC가 경로를 많이 줘도 앞에서 몇 개만");
    }

    /// 아무 연결도 받지 않는 주소 (열었다 닫은 포트)
    fn dead_address() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().to_string()
    }

    /// 한 번 `200 OK`를 답하는 HTTP 서버
    fn one_shot_server(body: &'static str) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_head(&mut stream);
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        (url, handle)
    }

    pub(crate) fn read_head(stream: &mut TcpStream) -> String {
        let mut reader = BufReader::new(stream);
        let mut head = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                return head;
            }
            head.push_str(&line);
        }
    }

    /// CONNECT를 받아 그대로 이어 주는 아주 작은 HTTP 프록시. 받은 CONNECT 대상들을 기록
    pub(crate) struct TestProxy {
        pub address: String,
        pub targets: Arc<Mutex<Vec<String>>>,
    }

    impl TestProxy {
        pub(crate) fn start(bind: &str) -> TestProxy {
            let listener = TcpListener::bind(bind).unwrap();
            let address = listener.local_addr().unwrap().to_string();
            let targets = Arc::new(Mutex::new(Vec::new()));
            let seen = targets.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    let seen = seen.clone();
                    std::thread::spawn(move || tunnel(stream, &seen));
                }
            });
            TestProxy { address, targets }
        }
    }

    fn tunnel(mut client: TcpStream, seen: &Mutex<Vec<String>>) {
        let head = read_head(&mut client);
        let mut words = head.split_whitespace();
        let (Some(method), Some(target)) = (words.next(), words.next()) else { return };
        seen.lock().unwrap().push(format!("{method} {target}"));
        if method != "CONNECT" {
            let _ = client.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n");
            return;
        }
        let Ok(upstream) = TcpStream::connect(target) else {
            let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n");
            return;
        };
        let _ = client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n");
        let (mut c2, mut u2) = (client.try_clone().unwrap(), upstream.try_clone().unwrap());
        let (mut client, mut upstream) = (client, upstream);
        let up = std::thread::spawn(move || {
            let _ = std::io::copy(&mut c2, &mut u2);
            let _ = u2.shutdown(std::net::Shutdown::Write);
        });
        let _ = std::io::copy(&mut upstream, &mut client);
        let _ = client.shutdown(std::net::Shutdown::Write);
        let _ = up.join();
    }

    #[test]
    fn failed_routes_fall_through_to_the_next_one() {
        let (url, server) = one_shot_server("direct");
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
        let dead = ureq::Proxy::new(&format!("http://{}", dead_address())).unwrap();
        let tries = AtomicUsize::new(0);
        let body = call_with(vec![Some(dead), None], |proxy| {
            tries.fetch_add(1, Ordering::SeqCst);
            agent.get(&url).config().proxy(proxy).build().call()
        })
        .unwrap()
        .body_mut()
        .read_to_string()
        .unwrap();
        server.join().unwrap();
        assert_eq!(body, "direct");
        assert_eq!(tries.load(Ordering::SeqCst), 2, "죽은 프록시 다음에 직접 연결");
    }

    #[test]
    fn every_route_failing_returns_the_last_error() {
        let agent: ureq::Agent = ureq::Agent::config_builder().build().new_agent();
        let url = format!("http://{}/", dead_address());
        let result = call_with(vec![None], |proxy| agent.get(&url).config().proxy(proxy).build().call());
        assert!(result.is_err());
    }

    #[test]
    fn requests_go_through_the_chosen_proxy() {
        let (url, server) = one_shot_server("tunneled");
        let test_proxy = TestProxy::start("127.0.0.1:0");
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().new_agent();
        let p = ureq::Proxy::new(&format!("http://{}", test_proxy.address)).unwrap();
        let mut response = call_with(vec![Some(p)], |proxy| agent.get(&url).config().proxy(proxy).build().call()).unwrap();
        let mut body = String::new();
        response.body_mut().as_reader().read_to_string(&mut body).unwrap();
        server.join().unwrap();
        assert_eq!(body, "tunneled");
        let target = url.trim_start_matches("http://").trim_end_matches('/');
        assert_eq!(test_proxy.targets.lock().unwrap()[0], format!("CONNECT {target}"));
    }
}
