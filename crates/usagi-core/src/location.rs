//! 실행 파일 위치: 내려받기·임시 폴더처럼 다른 파일이 쉽게 생기고 지워지는 곳인지.
//!
//! 그런 곳의 exe 경로로는 로그인할 때 실행, 시작 메뉴 바로 가기, Claude Code 상태 표시줄을 등록하지 않습니다.
//! 등록한 경로는 나중에 자동으로 실행되는데, 내려받기 폴더의 파일은 사용자가 지우거나 같은 이름의 다른 파일로
//! 바뀌기 쉽습니다. macOS에서 격리 표시가 붙은 앱을 내려받기 폴더에서 열면 임시 폴더(App Translocation)에서 실행됩니다.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transient {
    Downloads,
    Temp,
}

impl Transient {
    pub fn key(self) -> &'static str {
        match self {
            Transient::Downloads => "downloads",
            Transient::Temp => "temp",
        }
    }

    /// 무엇을 등록하지 않았는지와 이유 (`what`: "시작 시 실행" 등)
    pub fn blocked(self, what: (&str, &str)) -> String {
        let (ko, en) = match self {
            Transient::Downloads => ("내려받기 폴더", "the Downloads folder"),
            Transient::Temp => ("임시 폴더", "a temporary folder"),
        };
        crate::tr!(
            format!("{ko}에서 실행 중이라 {} 등록 안 함. usAgI를 고정 위치로 옮긴 뒤 다시 시도", what.0),
            format!("Running from {en}, so {} was not set up. Move usAgI to a permanent folder and try again", what.1)
        )
    }
}

/// 지금 실행 중인 exe의 위치 (한 번만 확인)
pub fn current() -> Option<Transient> {
    static CURRENT: std::sync::OnceLock<Option<Transient>> = std::sync::OnceLock::new();
    *CURRENT.get_or_init(|| std::env::current_exe().ok().and_then(|exe| transient(&exe)))
}

pub fn transient(exe: &Path) -> Option<Transient> {
    classify(exe, &temp_dirs(), downloads_dir().as_deref())
}

fn classify(exe: &Path, temp: &[PathBuf], downloads: Option<&Path>) -> Option<Transient> {
    if cfg!(target_os = "macos") && exe.components().any(|c| c.as_os_str() == "AppTranslocation") {
        return Some(Transient::Temp);
    }
    if temp.iter().any(|dir| is_under(exe, dir)) {
        Some(Transient::Temp)
    } else if downloads.is_some_and(|dir| is_under(exe, dir)) {
        Some(Transient::Downloads)
    } else {
        None
    }
}

/// `path`가 `dir` 안(하위 폴더 포함)에 있는지. 링크·대소문자(Windows)를 풀어서 비교
fn is_under(path: &Path, dir: &Path) -> bool {
    let (Ok(path), Ok(dir)) = (path.canonicalize(), dir.canonicalize()) else { return false };
    if cfg!(windows) {
        let lower = |p: &Path| p.to_string_lossy().to_lowercase();
        let (path, dir) = (lower(&path), lower(&dir));
        path.strip_prefix(dir.trim_end_matches('\\')).is_some_and(|rest| rest.starts_with('\\'))
    } else {
        path != dir && path.starts_with(&dir)
    }
}

fn temp_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![std::env::temp_dir()];
    if cfg!(windows) {
        dirs.extend(["TEMP", "TMP"].into_iter().filter_map(std::env::var_os).map(PathBuf::from));
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join("Temp"));
        }
        if let Some(root) = std::env::var_os("SystemRoot") {
            dirs.push(PathBuf::from(root).join("Temp"));
        }
    } else {
        dirs.extend(["/tmp", "/var/tmp"].map(PathBuf::from));
    }
    dirs.retain(|dir| dir.is_absolute());
    dirs
}

#[cfg(windows)]
fn downloads_dir() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath};
    // 사용자가 옮겼을 수 있으므로 알려진 폴더 API로 (없으면 기본 위치)
    let known = unsafe {
        let mut path: windows_sys::core::PWSTR = std::ptr::null_mut();
        let found = SHGetKnownFolderPath(&FOLDERID_Downloads, 0, std::ptr::null_mut(), &mut path) == 0 && !path.is_null();
        let result = found.then(|| {
            let len = (0..).take_while(|&i| *path.add(i) != 0).count();
            PathBuf::from(std::ffi::OsString::from_wide(std::slice::from_raw_parts(path, len)))
        });
        CoTaskMemFree(path.cast());
        result
    };
    known.or_else(|| std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join("Downloads")))
}

#[cfg(not(windows))]
fn downloads_dir() -> Option<PathBuf> {
    crate::logs::home_dir().map(|home| home.join("Downloads"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_downloads_and_temp_folders() {
        let root = tempfile::tempdir().unwrap();
        let (downloads, temp, tools) = (root.path().join("Downloads"), root.path().join("Temp"), root.path().join("Tools"));
        for dir in [&downloads, &temp, &tools, &downloads.join("usAgI-portable")] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let exe = |dir: &Path| {
            let path = dir.join("usagi.exe");
            std::fs::write(&path, "").unwrap();
            path
        };
        let temps = [temp.clone()];
        assert_eq!(classify(&exe(&downloads), &temps, Some(&downloads)), Some(Transient::Downloads));
        assert_eq!(classify(&exe(&downloads.join("usAgI-portable")), &temps, Some(&downloads)), Some(Transient::Downloads));
        assert_eq!(classify(&exe(&temp), &temps, Some(&downloads)), Some(Transient::Temp));
        assert_eq!(classify(&exe(&tools), &temps, Some(&downloads)), None);
        assert_eq!(classify(&tools.join("missing.exe"), &temps, Some(&downloads)), None, "없는 파일은 판단하지 않음");
        // 이름만 비슷한 옆 폴더는 안쪽이 아님
        std::fs::create_dir_all(root.path().join("Downloads2")).unwrap();
        assert_eq!(classify(&exe(&root.path().join("Downloads2")), &temps, Some(&downloads)), None);
    }

    #[test]
    fn the_system_temp_folder_is_transient() {
        let file = tempfile::NamedTempFile::new().unwrap();
        assert_eq!(transient(file.path()), Some(Transient::Temp));
    }

    #[test]
    fn explains_what_was_not_registered() {
        // 다른 테스트가 잠시 영어로 바꿀 수 있어 두 언어 모두 허용
        let text = Transient::Downloads.blocked(("시작 시 실행", "start at login"));
        assert!(text.starts_with("내려받기 폴더에서 실행 중이라 시작 시 실행 등록 안 함") || text.starts_with("Running from the Downloads folder, so start at login"), "{text}");
    }
}
