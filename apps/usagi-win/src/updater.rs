//! 자동 업데이트 (GitHub Releases).
//!
//! 1. 확인: `api.github.com/repos/<저장소>/releases/latest`의 태그를 지금 버전과 비교
//! 2. 받기: 릴리스의 `usagi.exe`와 `usagi.exe.sha256`을 받아 SHA-256이 맞는지 검증 → exe 옆 `usagi.exe.new`
//! 3. 적용: 실행 중인 exe는 이름은 바꿀 수 있으므로 `usagi.exe` → `usagi.exe.old`, `.new` → `usagi.exe`로 바꾸고
//!    새 exe를 `--after-update <pid>`로 띄운 뒤 지금 프로세스는 종료. 새 exe가 다음 시작 때 `.old`를 지움.
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
    let rest = url.strip_prefix("https://").ok_or("https 주소 아님")?;
    let (host, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((rest, "/".into()));
    // 업데이트는 GitHub에서만 받음 (리다이렉트는 WinHTTP가 https 안에서만 따라감)
    if !matches!(host.to_ascii_lowercase().as_str(), "api.github.com" | "github.com") {
        return Err("GitHub이 아닌 업데이트 주소".into());
    }
    unsafe {
        let agent = wide(&format!("usAgI/{} (+https://github.com/{REPO})", usagi_core::VERSION));
        let session = Handle(WinHttpOpen(agent.as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, null(), null(), 0));
        if session.0.is_null() {
            return Err("인터넷 연결 열기 실패".into());
        }
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000);
        let conn = Handle(WinHttpConnect(session.0, wide(host).as_ptr(), INTERNET_DEFAULT_HTTPS_PORT, 0));
        if conn.0.is_null() {
            return Err(format!("{host} 연결 실패"));
        }
        let req = Handle(WinHttpOpenRequest(conn.0, wide("GET").as_ptr(), wide(&path).as_ptr(), null(), null(), null(), WINHTTP_FLAG_SECURE));
        if req.0.is_null() {
            return Err("요청 만들기 실패".into());
        }
        let headers = wide(&format!("Accept: {accept}\r\nX-GitHub-Api-Version: 2022-11-28"));
        if WinHttpSendRequest(req.0, headers.as_ptr(), u32::MAX, null(), 0, 0, 0) == 0 || WinHttpReceiveResponse(req.0, null_mut()) == 0 {
            return Err("서버 연결 실패 (인터넷 연결 확인 필요)".into());
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
                return Err("내려받는 중 연결 끊김".into());
            }
            if avail == 0 {
                break;
            }
            let start = body.len();
            body.resize(start + avail as usize, 0);
            let mut read = 0u32;
            if WinHttpReadData(req.0, body[start..].as_mut_ptr() as *mut _, avail, &mut read) == 0 {
                return Err("내려받는 중 연결 끊김".into());
            }
            body.truncate(start + read as usize);
            if body.len() > 64 * 1024 * 1024 {
                return Err("파일 크기 초과".into());
            }
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
        404 => return Err("올라온 릴리스 없음".into()),
        403 | 429 => return Err("GitHub 요청 한도 초과. 잠시 뒤 다시 확인".into()),
        s => return Err(format!("업데이트 서버 응답 오류 ({s})")),
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
        return Err(format!("새 버전 수신 실패 ({status})"));
    }
    // 체크섬은 꼭 있어야 하고 일치해야 함
    let (s, text) = get(&rel.sha_url, "text/plain")?;
    let want = usagi_core::update::parse_sha256(&String::from_utf8_lossy(&text)).filter(|_| s == 200).ok_or("체크섬 수신 실패")?;
    if sha256_hex(&exe).as_deref() != Some(want.as_str()) {
        return Err("받은 파일 손상 (체크섬 불일치). 다시 시도 필요".into());
    }
    let staged = exe_path()?.with_extension("exe.new");
    std::fs::write(&staged, &exe).map_err(|e| format!("새 버전 저장 실패 (폴더 쓰기 권한 확인 필요): {e}"))?;
    Ok(staged)
}

/// 받아 둔 새 exe로 바꾸고 다시 시작. 성공하면 호출한 쪽이 지금 프로세스를 끝내야 함.
pub fn apply(staged: &Path) -> Result<(), String> {
    let exe = exe_path()?;
    let old = exe.with_extension("exe.old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&exe, &old).map_err(|e| format!("업데이트 적용 실패 (권한 확인 필요): {e}"))?;
    if let Err(e) = std::fs::rename(staged, &exe) {
        let _ = std::fs::rename(&old, &exe); // 되돌림
        return Err(format!("업데이트 적용 실패: {e}"));
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
            format!("새 버전 시작 실패: {e}")
        })
}

/// 업데이트 뒤 남은 예전 exe·받다 만 파일 정리 (시작할 때)
pub fn cleanup() {
    if let Ok(exe) = exe_path() {
        let _ = std::fs::remove_file(exe.with_extension("exe.old"));
    }
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
