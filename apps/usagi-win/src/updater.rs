//! 자동 업데이트 (GitHub Releases).
//!
//! 1. 확인: `api.github.com/repos/<저장소>/releases/latest`의 태그를 지금 버전과 비교
//! 2. 받기: `usagi.exe`를 받아 릴리스 본문 또는 기존 첨부의 체크섬과 서명으로 검증 → exe 옆 `usagi.exe.new`
//! 3. 적용: 실행 중인 exe는 이름은 바꿀 수 있으므로 `usagi.exe` → `usagi.exe.old`, `.new` → `usagi.exe`로 바꾸고
//!    새 exe를 `--after-update <pid>`로 띄운 뒤 지금 프로세스는 종료. `.old`는 복구용으로 보존.
//!
//! 통신(인터넷 옵션의 프록시·PAC 포함)과 검증은 코어의 [`usagi_core::update`]. 여기는 받은 파일을 두고 바꿔 끼우기만.

use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

pub use usagi_core::update::{Release, REPO};

const ASSET: &str = "usagi.exe";

/// 최신 릴리스 확인. 지금보다 새 버전이면 Some.
pub fn check() -> Result<Option<Release>, String> {
    usagi_core::update::check(ASSET)
}

fn exe_path() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| e.to_string())
}

fn save_error(error: std::io::Error) -> String {
    usagi_core::tr!(
        format!("새 버전 저장 실패 (폴더 쓰기 권한 확인 필요): {error}"),
        format!("Couldn't save the new version (check folder write permission): {error}")
    )
}

/// 새 exe를 받아 검증한 뒤 exe 옆 `usagi.exe.new`로 둠
pub fn download(rel: &Release) -> Result<PathBuf, String> {
    let exe = exe_path()?;
    let parent = exe.parent().ok_or_else(|| usagi_core::tr!("앱 폴더 없음", "App folder missing").to_string())?;
    let staged = exe.with_extension("exe.new");
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(save_error)?;
    let file = temporary.as_file_mut();
    usagi_core::update::download(rel, file)?;
    file.rewind().map_err(save_error)?;
    let mut magic = [0; 2];
    if file.metadata().map_err(save_error)?.len() < 100 * 1024 || file.read_exact(&mut magic).is_err() || &magic != b"MZ" {
        return Err(usagi_core::tr!("새 버전 수신 실패 (실행 파일이 아님)", "Couldn't download the new version (not an executable)").into());
    }
    file.rewind().map_err(save_error)?;
    rel.verify_asset(file, ASSET, usagi_core::update::fetch_text)?;
    temporary.persist(&staged).map_err(|error| save_error(error.error))?;
    Ok(staged)
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
