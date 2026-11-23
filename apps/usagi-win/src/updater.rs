//! 자동 업데이트 (GitHub Releases).
//!
//! 1. 확인: `api.github.com/repos/<저장소>/releases/latest`의 태그를 지금 버전과 비교
//! 2. 받기: `usagi.exe`를 받아 릴리스 본문 또는 기존 첨부의 체크섬과 서명으로 검증 → exe 옆 `usagi.exe.new`
//! 3. 적용: 실행 중인 exe는 이름은 바꿀 수 있으므로 `usagi.exe` → `usagi.exe.old`, `.new` → `usagi.exe`로 바꾸고
//!    새 exe를 `--after-update <pid>`로 띄운 뒤 지금 프로세스는 종료. `.old`는 복구용으로 보존.
//!
//! 통신은 Windows 기본 WinHTTP, 해시는 Windows 기본 BCrypt (추가 라이브러리 없음).

use crate::win::wide;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Networking::WinHttp::*;
use windows_sys::Win32::Security::Cryptography::{BCryptHash, BCRYPT_SHA256_ALG_HANDLE};

/// 업데이트를 받을 저장소 ("owner/name"). 릴리스 빌드(GitHub Actions)는 자기 저장소를 넣어 빌드함.
pub const REPO: &str = match option_env!("USAGI_UPDATE_REPO") {
    Some(r) => r,
    None => "Ba-koD/usAgI",
};

pub use usagi_core::update::Release;

// ------------------------------------------------------------------ HTTP (WinHTTP)

struct Handle(*mut core::ffi::c_void);
impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

/// HTTPS GET. (상태 코드, 본문). 리다이렉트는 WinHTTP가 따라감.
fn get(url: &str, accept: &str) -> Result<(u32, Vec<u8>), String> {
    let rest = url.strip_prefix("https://").ok_or(usagi_core::tr!("https 주소 아님", "Not an https address"))?;
    let (host, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((rest, "/".into()));
    // 업데이트는 GitHub에서만 받음 (리다이렉트는 WinHTTP가 https 안에서만 따라감)
    if !matches!(host.to_ascii_lowercase().as_str(), "api.github.com" | "github.com") {
        return Err(usagi_core::tr!("GitHub이 아닌 업데이트 주소", "Update address is not on GitHub").into());
    }
    unsafe {
        let agent = wide(&format!("usAgI/{} (+https://github.com/{REPO})", usagi_core::VERSION));
        let session = Handle(WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, null(), null(), 0));
        if session.0.is_null() {
            return Err(usagi_core::tr!("인터넷 연결 열기 실패", "Couldn't open an internet connection").into());
        }
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000);
        let conn = Handle(WinHttpConnect(session.0, wide(host).as_ptr(), INTERNET_DEFAULT_HTTPS_PORT, 0));
        if conn.0.is_null() {
            return Err(usagi_core::tr!(format!("{host} 연결 실패"), format!("Couldn't connect to {host}")));
        }
        let req = Handle(WinHttpOpenRequest(conn.0, wide("GET").as_ptr(), wide(&path).as_ptr(), null(), null(), null(), WINHTTP_FLAG_SECURE));
        if req.0.is_null() {
            return Err(usagi_core::tr!("요청 만들기 실패", "Couldn't create the request").into());
        }
        let headers = wide(&format!("Accept: {accept}\r\nX-GitHub-Api-Version: 2022-11-28"));
        if WinHttpSendRequest(req.0, headers.as_ptr(), u32::MAX, null(), 0, 0, 0) == 0 || WinHttpReceiveResponse(req.0, null_mut()) == 0 {
            return Err(usagi_core::tr!("서버 연결 실패 (인터넷 연결 확인 필요)", "Couldn't reach the server (check the internet connection)").into());
        }
        let mut status: u32 = 0;
        let mut len = 4u32;
        WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            null(),
            &mut status as *mut u32 as *mut _,
            &mut len,
            null_mut(),
        );
        let mut body = Vec::new();
        loop {
            let mut avail = 0u32;
            if WinHttpQueryDataAvailable(req.0, &mut avail) == 0 {
                return Err(usagi_core::tr!("내려받는 중 연결 끊김", "Connection lost during download").into());
            }
            if avail == 0 {
                break;
            }
            let start = body.len();
            if avail as usize > 64 * 1024 * 1024 - start {
                return Err(usagi_core::tr!("파일 크기 초과", "File too large").into());
            }
            body.resize(start + avail as usize, 0);
            let mut read = 0u32;
            if WinHttpReadData(req.0, body[start..].as_mut_ptr() as *mut _, avail, &mut read) == 0 {
                return Err(usagi_core::tr!("내려받는 중 연결 끊김", "Connection lost during download").into());
            }
            body.truncate(start + read as usize);
        }
        Ok((status, body))
    }
}

pub fn sha256_hex(data: &[u8]) -> Option<String> {
    let mut out = [0u8; 32];
    let st = unsafe { BCryptHash(BCRYPT_SHA256_ALG_HANDLE, null(), 0, data.as_ptr(), data.len() as u32, out.as_mut_ptr(), 32) };
    (st == 0).then(|| out.iter().map(|b| format!("{b:02x}")).collect())
}

// ------------------------------------------------------------------ 확인·받기·적용

/// 최신 릴리스 확인. 지금보다 새 버전이면 Some.
pub fn check() -> Result<Option<Release>, String> {
    let (status, body) = get(&usagi_core::update::latest_api(REPO), "application/vnd.github+json")?;
    match status {
        200 => {}
        404 => return Err(usagi_core::tr!("올라온 릴리스 없음", "No release published").into()),
        403 | 429 => return Err(usagi_core::tr!("GitHub 요청 한도 초과. 잠시 뒤 다시 확인", "GitHub request limit reached. Check again later").into()),
        s => return Err(usagi_core::tr!(format!("업데이트 서버 응답 오류 ({s})"), format!("Update server error ({s})"))),
    }
    usagi_core::update::parse_latest(&body, usagi_core::VERSION, REPO, "usagi.exe")
}

fn exe_path() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| e.to_string())
}

/// 새 exe를 받아 검증한 뒤 exe 옆 `usagi.exe.new`로 둠
pub fn download(rel: &Release) -> Result<PathBuf, String> {
    let (status, exe) = get(&rel.asset_url, "application/octet-stream")?;
    if status != 200 || exe.len() < 100 * 1024 || !exe.starts_with(b"MZ") {
        return Err(usagi_core::tr!(format!("새 버전 수신 실패 ({status})"), format!("Couldn't download the new version ({status})")));
    }
    // 체크섬은 꼭 있어야 하고 일치해야 함
    let want = rel.verification.checksum(verification_text)?;
    if sha256_hex(&exe).as_deref() != Some(want.as_str()) {
        return Err(usagi_core::tr!("받은 파일 손상 (체크섬 불일치). 다시 시도 필요", "Downloaded file is damaged (checksum mismatch). Try again").into());
    }
    rel.verify_signature(&exe, "usagi.exe", verification_text)?;
    let staged = exe_path()?.with_extension("exe.new");
    std::fs::write(&staged, &exe).map_err(|e| usagi_core::tr!(format!("새 버전 저장 실패 (폴더 쓰기 권한 확인 필요): {e}"), format!("Couldn't save the new version (check folder write permission): {e}")))?;
    Ok(staged)
}

fn verification_text(url: &str) -> Result<String, String> {
    let (status, text) = get(url, "text/plain")?;
    if status != 200 {
        return Err(usagi_core::tr!("검증 정보 수신 실패", "Couldn't download verification information").into());
    }
    Ok(String::from_utf8_lossy(&text).into_owned())
}

/// 받아 둔 새 exe로 바꾸고 다시 시작. 성공하면 호출한 쪽이 지금 프로세스를 끝내야 함.
pub fn apply(staged: &Path) -> Result<(), String> {
    let exe = exe_path()?;
    let old = exe.with_extension("exe.old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| usagi_core::tr!(format!("업데이트 적용 실패 (권한 확인 필요): {e}"), format!("Couldn't apply the update (check permissions): {e}")))?;
    if let Err(e) = std::fs::rename(staged, &exe) {
        let _ = std::fs::rename(&old, &exe); // 되돌림
        return Err(usagi_core::tr!(format!("업데이트 적용 실패: {e}"), format!("Couldn't apply the update: {e}")));
    }
    std::process::Command::new(&exe)
        .arg("--after-update")
        .arg(std::process::id().to_string())
        .spawn()
        .map(|_| ())
        .map_err(|e| {
            // 새 exe를 못 띄우면 원래대로
            let _ = std::fs::rename(&exe, staged);
            let _ = std::fs::rename(&old, &exe);
            usagi_core::tr!(format!("새 버전 시작 실패: {e}"), format!("Couldn't start the new version: {e}"))
        })
}

/// `--after-update <pid>`: 예전 프로세스가 끝날 때까지 잠깐 기다림 (중복 실행 방지 뮤텍스를 넘겨받으려고)
pub fn wait_for(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
    unsafe {
        let h = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
        if !h.is_null() {
            WaitForSingleObject(h, 15_000);
            CloseHandle(h);
        }
    }
}
