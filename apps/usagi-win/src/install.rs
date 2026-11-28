//! 설치 위치로 옮기기: 내려받기·임시 폴더에서 실행 중인 usagi.exe를 %LOCALAPPDATA%\Programs\usAgI
//! (scripts/install-windows.ps1과 같은 위치)로 복사하고 그 사본으로 다시 시작합니다.
//!
//! 내려받기·임시 폴더의 exe 경로로는 시작 시 실행·시작 메뉴 바로 가기·Claude Code 실시간 연결을 등록하지 않으므로
//! (usagi_core::location), 설정 창에서 이유를 보여 주고 이 단추를 제공합니다. 데이터는 그대로 %APPDATA%\usAgI.

use std::path::PathBuf;
use std::process::Command;

/// 옮긴 사본의 첫 실행 표시: `--installed <예전 pid>`
pub const ARG: &str = "--installed";

fn dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join("Programs").join("usAgI"))
}

/// 옮길 수 있으면 설치 위치. 포터블 사본은 data 폴더째 옮겨야 하므로 제외
pub fn available() -> Option<PathBuf> {
    if usagi_core::config::is_portable() || usagi_core::location::current().is_none() {
        return None;
    }
    dir()
}

fn failed(error: impl std::fmt::Display) -> String {
    usagi_core::tr!(
        format!("설치 위치로 옮기기 실패 ({error}). 설치 위치의 usAgI가 실행 중이면 끝낸 뒤 다시 시도"),
        format!("Couldn't move usAgI to the install folder ({error}). If usAgI is running from there, quit it and try again")
    )
}

/// 복사하고 새 사본을 띄움. 성공하면 호출한 쪽이 지금 프로세스를 끝내야 함 (새 사본은 끝나길 기다렸다가 시작)
pub fn copy_and_restart() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(failed)?;
    let dir = available().ok_or_else(|| failed(usagi_core::tr!("옮길 수 없는 실행 위치", "not available for this copy")))?;
    std::fs::create_dir_all(&dir).map_err(failed)?;
    let target = dir.join("usagi.exe");
    // 임의 이름으로 새로 만든 파일에 복사한 뒤 바꿔 넣음: 복사가 중간에 끊겨도 기존 설치본이 깨지지 않음
    let staged = tempfile::Builder::new().prefix(".install-").suffix(".tmp").tempfile_in(&dir).map_err(failed)?.into_temp_path();
    std::fs::copy(&exe, &staged).map_err(failed)?;
    staged.persist(&target).map_err(|error| failed(error.error))?;
    Command::new(&target)
        .arg(ARG)
        .arg(std::process::id().to_string())
        .current_dir(&dir)
        .spawn()
        .map_err(failed)?;
    Ok(target)
}
