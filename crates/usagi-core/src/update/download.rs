//! 업데이트 통신 (Windows·macOS 공통): 최신 릴리스 확인, 새 버전 파일과 검증 정보 받기.
//!
//! - 처음 주소는 GitHub(`api.github.com`, `github.com`)의 https만. 리다이렉트도 https만, 최대 5번
//!   (GitHub이 릴리스 파일을 다른 서버로 넘김)
//! - 리다이렉트마다 프록시를 다시 골라요 ([`crate::net`], PAC는 호스트마다 답이 다를 수 있음)
//! - 받는 크기는 [`MAX_ASSET_BYTES`]까지. 받은 파일 검증은 [`Release::verify_asset`]
//!
//! 받은 파일을 어디에 두고 어떻게 바꿔 끼우는지는 플랫폼마다 달라서 각 앱의 updater가 해요.

use super::{latest_api, parse_latest, Release, MAX_ASSET_BYTES};
use std::io::{Read, Write};
use std::sync::LazyLock;
use std::time::Duration;

/// 업데이트를 받을 저장소 ("owner/name"). 릴리스 빌드(GitHub Actions)는 자기 저장소를 넣어 빌드함.
pub const REPO: &str = match option_env!("USAGI_UPDATE_REPO") {
    Some(r) => r,
    None => "Ba-koD/usAgI",
};

const MAX_REDIRECTS: usize = 5;
/// 체크섬·서명 파일 크기 한도
const MAX_TEXT_BYTES: u64 = 64 * 1024;

/// 이름 찾기·연결 시간 제한
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// 요청 하나(리다이렉트 한 번)의 전체 시간 제한: 작은 응답(릴리스 정보·검증 정보)과 새 버전 파일.
/// ureq의 응답 대기 제한(`timeout_recv_response`)은 본문 읽기까지 끊어 느린 연결에서 다운로드가 실패하므로 쓰지 않고,
/// 처음부터 본문 끝까지를 재는 전체 제한(`timeout_global`)을 요청마다 줌
const SMALL_TIMEOUT: Duration = Duration::from_secs(60);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

static AGENT: LazyLock<ureq::Agent> = LazyLock::new(|| {
    let config = ureq::Agent::config_builder()
        .https_only(true)
        // 리다이렉트는 직접 따라감 (주소마다 프록시를 고르고 https만 허용)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_resolve(Some(CONNECT_TIMEOUT))
        .timeout_connect(Some(CONNECT_TIMEOUT));
    crate::net::tls(config).build().new_agent()
});

/// 최신 릴리스 확인. 지금보다 새 버전이고 이 플랫폼 파일(`asset`)이 있으면 Some.
pub fn check(asset: &str) -> Result<Option<Release>, String> {
    latest(asset, crate::VERSION, &[])
}

fn latest(asset: &str, current: &str, headers: &[(&str, &str)]) -> Result<Option<Release>, String> {
    let mut body = Vec::new();
    let status = get(&latest_api(REPO), "application/vnd.github+json", headers, &mut body, MAX_ASSET_BYTES, SMALL_TIMEOUT)?;
    match status {
        200 => parse_latest(&body, current, REPO, asset),
        404 => Err(crate::tr!("올라온 릴리스 없음", "No release published").into()),
        403 | 429 => Err(crate::tr!("GitHub 요청 한도 초과. 잠시 뒤 다시 확인", "GitHub request limit reached. Check again later").into()),
        s => Err(crate::tr!(format!("업데이트 서버 응답 오류 ({s})"), format!("Update server error ({s})"))),
    }
}

/// 새 버전 파일을 `out`에 받음. 받은 뒤 [`Release::verify_asset`]으로 검증해야 함
pub fn download(release: &Release, out: &mut impl Write) -> Result<(), String> {
    let status = get(&release.asset_url, "application/octet-stream", &[], out, MAX_ASSET_BYTES, DOWNLOAD_TIMEOUT)?;
    if status != 200 {
        return Err(crate::tr!(format!("새 버전 수신 실패 ({status})"), format!("Couldn't download the new version ({status})")));
    }
    Ok(())
}

/// 체크섬·서명 같은 검증 정보 받기 ([`Release::verify_asset`]에 넘김)
pub fn fetch_text(url: &str) -> Result<String, String> {
    let mut body = Vec::new();
    let status = get(url, "text/plain", &[], &mut body, MAX_TEXT_BYTES, SMALL_TIMEOUT)?;
    if status != 200 {
        return Err(crate::tr!("검증 정보 수신 실패", "Couldn't download verification information").into());
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn get(url: &str, accept: &str, headers: &[(&str, &str)], out: &mut dyn Write, limit: u64, budget: Duration) -> Result<u16, String> {
    if !github_url(url) {
        return Err(crate::tr!("GitHub이 아닌 업데이트 주소", "Update address is not on GitHub").into());
    }
    follow(&AGENT, url, accept, headers, out, limit, budget, true)
}

/// GitHub의 https 주소인지 (업데이트 확인과 파일 받기를 시작하는 주소)
fn github_url(url: &str) -> bool {
    match crate::net::scheme_host(url) {
        Some((scheme, host)) => scheme == "https" && matches!(host.as_str(), "api.github.com" | "github.com"),
        None => false,
    }
}

fn user_agent() -> String {
    format!("usAgI/{} ({}; +https://github.com/{REPO})", crate::VERSION, std::env::consts::OS)
}

/// GET하고 리다이렉트를 따라감. 200이면 본문을 `out`에 쓰고, 상태 코드를 돌려줌.
/// `headers`는 처음 주소에만 보냄 (다른 서버로 넘어갈 때 따라가지 않게). `budget`은 요청 하나의 전체 시간 (본문 끝까지)
#[allow(clippy::too_many_arguments)]
fn follow(
    agent: &ureq::Agent,
    url: &str,
    accept: &str,
    headers: &[(&str, &str)],
    out: &mut dyn Write,
    limit: u64,
    budget: Duration,
    https_only: bool,
) -> Result<u16, String> {
    let agent_header = user_agent();
    let mut url = url.to_string();
    for hop in 0..=MAX_REDIRECTS {
        let extra = if hop == 0 { headers } else { &[] };
        let mut response = crate::net::call(&url, |proxy| {
            let mut request = agent
                .get(&url)
                .config()
                .proxy(proxy)
                .timeout_global(Some(budget))
                .build()
                .header("User-Agent", &agent_header)
                .header("Accept", accept)
                .header("X-GitHub-Api-Version", "2022-11-28");
            for &(name, value) in extra {
                request = request.header(name, value);
            }
            request.call()
        })
        .map_err(|error| {
            crate::tr!(
                format!("업데이트 서버 연결 실패 (인터넷·프록시 연결 확인 필요): {error}"),
                format!("Couldn't reach the update server (check the internet or proxy connection): {error}")
            )
        })?;
        let status = response.status().as_u16();
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            let location = response.headers().get("location").and_then(|v| v.to_str().ok()).unwrap_or("");
            let next = redirect_target(&url, location)
                .ok_or_else(|| crate::tr!("업데이트 서버의 이동 주소 오류", "Invalid redirect from the update server").to_string())?;
            if https_only && !next.starts_with("https://") {
                return Err(crate::tr!("https가 아닌 주소로 이동해 중단", "Stopped: redirected to a non-https address").into());
            }
            url = next;
            continue;
        }
        if status == 200 {
            copy_limited(response.body_mut().with_config().limit(u64::MAX).reader(), out, limit)?;
        }
        return Ok(status);
    }
    Err(crate::tr!("업데이트 서버의 이동이 너무 많음", "Too many redirects from the update server").into())
}

/// 리다이렉트 `Location`을 절대 주소로
fn redirect_target(base: &str, location: &str) -> Option<String> {
    let location = location.trim();
    if location.is_empty() {
        return None;
    }
    if location.starts_with("https://") || location.starts_with("http://") {
        return Some(location.to_string());
    }
    let (scheme, rest) = base.split_once("://")?;
    if let Some(network) = location.strip_prefix("//") {
        return Some(format!("{scheme}://{network}"));
    }
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(authority_end);
    if location.starts_with('/') {
        return Some(format!("{scheme}://{authority}{location}"));
    }
    let path = path.split(['?', '#']).next().unwrap_or("");
    let dir = path.rsplit_once('/').map_or("", |(dir, _)| dir);
    Some(format!("{scheme}://{authority}{dir}/{location}"))
}

/// 본문을 크기 한도 안에서 옮김
fn copy_limited(mut body: impl Read, out: &mut dyn Write, limit: u64) -> Result<(), String> {
    let mut remaining = limit;
    let mut buf = vec![0; 64 * 1024];
    loop {
        let count = match body.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(crate::tr!("내려받는 중 연결 끊김", "Connection lost during download").into()),
        };
        remaining = remaining.checked_sub(count as u64).ok_or_else(|| crate::tr!("파일 크기 초과", "File too large").to_string())?;
        out.write_all(&buf[..count]).map_err(|error| {
            crate::tr!(
                format!("새 버전 저장 실패 (폴더 쓰기 권한 확인 필요): {error}"),
                format!("Couldn't save the new version (check folder write permission): {error}")
            )
        })?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::tests::read_head;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// 요청 머리를 기록하고 `respond(경로)`로 답하는 HTTP 서버
    fn server(respond: impl Fn(&str) -> String + Send + 'static) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let heads = Arc::new(Mutex::new(Vec::new()));
        let seen = heads.clone();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let head = read_head(&mut stream);
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                seen.lock().unwrap().push(head);
                let _ = stream.write_all(respond(&path).as_bytes());
            }
        });
        (base, heads)
    }

    fn ok(body: &str) -> String {
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    fn redirect(location: &str) -> String {
        format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    }

    /// 테스트 요청의 전체 시간 제한
    const SECS: Duration = Duration::from_secs(5);

    fn plain_agent() -> ureq::Agent {
        ureq::Agent::config_builder().max_redirects(0).http_status_as_error(false).build().new_agent()
    }

    /// 머리는 바로, 본문은 `delay`마다 1바이트씩 `bytes`개 보내는 서버 (느린 연결)
    fn trickle_server(bytes: usize, delay: Duration) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/slow", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_head(&mut stream);
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {bytes}\r\nConnection: close\r\n\r\n");
            for _ in 0..bytes {
                std::thread::sleep(delay);
                if stream.write_all(b"x").is_err() {
                    return;
                }
            }
        });
        url
    }

    #[test]
    fn slow_bodies_finish_within_the_call_budget() {
        // 머리를 받은 뒤 본문이 1.5초에 걸쳐 오는 경우: 예전 설정(응답 대기 제한)은 여기서 끊겼음
        let url = trickle_server(6, Duration::from_millis(250));
        let mut body = Vec::new();
        assert_eq!(follow(&plain_agent(), &url, "*/*", &[], &mut body, 1024, Duration::from_secs(5), false), Ok(200));
        assert_eq!(body, b"xxxxxx");
        // 전체 제한은 본문 읽기까지 적용
        let url = trickle_server(20, Duration::from_millis(250));
        let error = follow(&plain_agent(), &url, "*/*", &[], &mut Vec::new(), 1024, Duration::from_secs(1), false).unwrap_err();
        assert!(error.contains("연결 끊김") || error.contains("연결 실패"), "{error}");
    }

    #[test]
    fn only_github_https_addresses_start_an_update() {
        assert!(github_url("https://api.github.com/repos/o/r/releases/latest"));
        assert!(github_url("https://github.com/o/r/releases/download/v1/usagi.exe"));
        assert!(!github_url("http://github.com/o/r"));
        assert!(!github_url("https://github.com.evil.example/o/r"));
        assert!(!github_url("https://evil.example/github.com"));
        assert!(get("https://example.com/x", "*/*", &[], &mut Vec::new(), 10, SECS).is_err());
    }

    #[test]
    fn redirect_locations_become_absolute() {
        let base = "https://github.com/o/r/releases/download/v1/usagi.exe?x=1";
        assert_eq!(redirect_target(base, "https://cdn.example/a").as_deref(), Some("https://cdn.example/a"));
        assert_eq!(redirect_target(base, "//cdn.example/a").as_deref(), Some("https://cdn.example/a"));
        assert_eq!(redirect_target(base, "/other").as_deref(), Some("https://github.com/other"));
        assert_eq!(redirect_target(base, "next.exe").as_deref(), Some("https://github.com/o/r/releases/download/v1/next.exe"));
        assert_eq!(redirect_target(base, " "), None);
    }

    #[test]
    fn follows_redirects_and_keeps_extra_headers_on_the_first_server() {
        let (base, heads) = server(|path| match path {
            "/start" => redirect("/file"),
            "/file" => ok("payload"),
            _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
        });
        let mut body = Vec::new();
        let status = follow(&plain_agent(), &format!("{base}/start"), "*/*", &[("Authorization", "Bearer test")], &mut body, 1024, SECS, false).unwrap();
        assert_eq!((status, body.as_slice()), (200, b"payload".as_slice()));
        let heads = heads.lock().unwrap();
        assert!(heads[0].to_ascii_lowercase().contains("authorization: bearer test"));
        assert!(!heads[1].to_ascii_lowercase().contains("authorization"), "다른 주소로 넘어가면 보내지 않음");
        assert!(heads[1].contains("usAgI/"), "User-Agent는 계속");
    }

    #[test]
    fn redirect_rules_and_limits() {
        let (base, _) = server(|path| match path {
            "/loop" => redirect("/loop"),
            "/to-http" => redirect("http://example.com/x"),
            "/big" => ok(&"x".repeat(2048)),
            _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnope".into(),
        });
        let agent = plain_agent();
        let mut sink = Vec::new();
        assert!(follow(&agent, &format!("{base}/loop"), "*/*", &[], &mut sink, 1024, SECS, false).unwrap_err().contains("이동"));
        assert!(follow(&agent, &format!("{base}/to-http"), "*/*", &[], &mut sink, 1024, SECS, true).unwrap_err().contains("https"));
        assert!(follow(&agent, &format!("{base}/big"), "*/*", &[], &mut sink, 1024, SECS, false).unwrap_err().contains("크기"));
        let mut body = Vec::new();
        assert_eq!(follow(&agent, &format!("{base}/missing"), "*/*", &[], &mut body, 1024, SECS, false), Ok(404));
        assert!(body.is_empty(), "200이 아니면 본문을 쓰지 않음");
    }

    #[test]
    fn unreachable_server_is_a_connection_error() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        drop(listener);
        let error = follow(&plain_agent(), &url, "*/*", &[], &mut Vec::new(), 1024, SECS, false).unwrap_err();
        assert!(error.contains("연결 실패"), "{error}");
    }

    /// 이 OS의 릴리스 파일
    fn platform_asset() -> &'static str {
        if cfg!(windows) {
            "usagi.exe"
        } else {
            "usAgI-mac.zip"
        }
    }

    /// CI의 GITHUB_TOKEN이 있으면 API 요청 한도를 피하려고 붙임 (처음 주소에만 감)
    fn token_header() -> Option<String> {
        std::env::var("GITHUB_TOKEN").ok().filter(|t| !t.is_empty()).map(|t| format!("Bearer {t}"))
    }

    /// 실제 GitHub 최신 릴리스를 확인 → 받기 → 서명 검증 (앱의 업데이트와 같은 코드)
    fn real_update_round_trip() -> u64 {
        let token = token_header();
        let headers: Vec<(&str, &str)> = token.iter().map(|t| ("Authorization", t.as_str())).collect();
        let release = latest(platform_asset(), "0.0.0", &headers).expect("최신 릴리스 확인").expect("릴리스 있음");
        let mut file = tempfile::tempfile().unwrap();
        download(&release, &mut file).expect("새 버전 받기");
        use std::io::Seek;
        let size = file.stream_position().unwrap();
        file.rewind().unwrap();
        release.verify_asset(&file, platform_asset(), fetch_text).expect("체크섬·서명 검증");
        size
    }

    #[test]
    #[ignore = "네트워크: CI에서 실행"]
    fn real_release_downloads_and_verifies() {
        let size = real_update_round_trip();
        assert!(size > 100 * 1024, "{size} bytes");
    }

    /// 시스템 설정을 건드리지 않고 프록시 경유를 확인 (개발 PC용): 환경 변수 프록시로 실제 업데이트
    #[test]
    #[ignore = "네트워크: 개발 PC에서 직접 실행"]
    fn real_update_goes_through_an_env_proxy() {
        let proxy = crate::net::tests::TestProxy::start("127.0.0.1:0");
        std::env::set_var("HTTPS_PROXY", format!("http://{}", proxy.address));
        real_update_round_trip();
        std::env::remove_var("HTTPS_PROXY");
        let targets = proxy.targets.lock().unwrap().clone();
        assert!(targets.iter().any(|t| t == "CONNECT api.github.com:443"), "{targets:?}");
        assert!(targets.iter().any(|t| t == "CONNECT github.com:443"), "{targets:?}");
    }

    /// CI 전용: OS 프록시를 테스트 프록시로 맞춘 뒤 실제 업데이트가 그 프록시를 거치는지 확인.
    /// macOS는 CI가 `networksetup`으로, Windows는 이 테스트가 인터넷 옵션을 바꿈 (`USAGI_PROXY_E2E=127.0.0.1:포트`)
    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    #[ignore = "CI에서 시스템 프록시를 설정한 뒤 실행"]
    fn real_update_goes_through_the_system_proxy() {
        let address = std::env::var("USAGI_PROXY_E2E").expect("USAGI_PROXY_E2E");
        let proxy = crate::net::tests::TestProxy::start(&address);
        #[cfg(windows)]
        let _restore = windows_proxy::set(&address);
        crate::net::clear_cache();
        assert_eq!(
            crate::net::proxies_for("https://api.github.com/").iter().map(|p| p.as_ref().map(|p| format!("{}:{}", p.host(), p.port()))).collect::<Vec<_>>(),
            vec![Some(address.clone())],
            "OS 프록시 설정을 읽음"
        );
        real_update_round_trip();
        let targets = proxy.targets.lock().unwrap().clone();
        assert!(targets.iter().any(|t| t == "CONNECT api.github.com:443"), "{targets:?}");
        assert!(targets.iter().any(|t| t == "CONNECT github.com:443"), "{targets:?}");
    }

    /// Windows 인터넷 옵션(LAN 설정)의 수동 프록시를 바꾸고, 끝나면 직접 연결로 되돌림
    #[cfg(windows)]
    mod windows_proxy {
        use std::ptr::null;
        use windows_sys::Win32::Networking::WinInet::*;

        pub struct Restore;

        impl Drop for Restore {
            fn drop(&mut self) {
                apply(PROXY_TYPE_DIRECT, None);
            }
        }

        pub fn set(address: &str) -> Restore {
            assert!(apply(PROXY_TYPE_DIRECT | PROXY_TYPE_PROXY, Some(address)), "인터넷 옵션 바꾸기 실패");
            Restore
        }

        fn apply(flags: u32, server: Option<&str>) -> bool {
            let mut server: Vec<u16> = server.unwrap_or("").encode_utf16().chain(Some(0)).collect();
            let mut bypass: Vec<u16> = "<local>".encode_utf16().chain(Some(0)).collect();
            let mut options = [
                INTERNET_PER_CONN_OPTIONW { dwOption: INTERNET_PER_CONN_FLAGS, Value: INTERNET_PER_CONN_OPTIONW_0 { dwValue: flags } },
                INTERNET_PER_CONN_OPTIONW { dwOption: INTERNET_PER_CONN_PROXY_SERVER, Value: INTERNET_PER_CONN_OPTIONW_0 { pszValue: server.as_mut_ptr() } },
                INTERNET_PER_CONN_OPTIONW { dwOption: INTERNET_PER_CONN_PROXY_BYPASS, Value: INTERNET_PER_CONN_OPTIONW_0 { pszValue: bypass.as_mut_ptr() } },
            ];
            let list = INTERNET_PER_CONN_OPTION_LISTW {
                dwSize: std::mem::size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
                pszConnection: std::ptr::null_mut(),
                dwOptionCount: options.len() as u32,
                dwOptionError: 0,
                pOptions: options.as_mut_ptr(),
            };
            unsafe {
                let ok = InternetSetOptionW(null(), INTERNET_OPTION_PER_CONNECTION_OPTION, (&list as *const INTERNET_PER_CONN_OPTION_LISTW).cast(), list.dwSize) != 0;
                InternetSetOptionW(null(), INTERNET_OPTION_SETTINGS_CHANGED, null(), 0);
                InternetSetOptionW(null(), INTERNET_OPTION_REFRESH, null(), 0);
                ok
            }
        }
    }
}
