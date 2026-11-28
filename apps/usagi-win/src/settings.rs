//! Windows message-loop adapter for the shared, on-demand settings process.
use flate2::read::GzDecoder;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use usagi_core::settings_window::SettingsProgram;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub use usagi_core::settings_window::SettingsWindow;
pub const WM_IPC: u32 = WM_APP + 11;

pub fn open(host: HWND) -> std::io::Result<SettingsWindow> {
    let host = host as isize;
    SettingsWindow::open(program(&std::env::current_exe()?)?, move || unsafe {
        PostMessageW(host as HWND, WM_IPC, 0, 0);
    })
}

pub fn run_standalone() {
    let result = std::env::current_exe().and_then(|exe| program(&exe)).and_then(SettingsProgram::run);
    match result {
        Ok(status) if !status.success() => std::process::exit(status.code().unwrap_or(1)),
        Ok(_) => {}
        Err(error) => {
            println!("{}", serde_json::json!({"cmd":"settingsError","error":error.to_string()}));
            std::process::exit(1);
        }
    }
}

const SETTINGS_EXE: &str = "usagi-settings.exe";
// scripts/package-portable.ps1 appends gzip bytes, their LE u64 length, and
// this marker after the PE image, so Windows does not map the UI at startup.
const MARKER: &[u8; 16] = b"USAGISETTINGS001";
const FOOTER_SIZE: u64 = 8 + MARKER.len() as u64;
const MAX_UI_SIZE: u64 = 64 * 1024 * 1024;

/// exe 옆의 usagi-settings.exe를 써도 되는 빌드: 개발(디버그)·테스트(mock) 빌드만.
/// 배포 exe는 내려받기 폴더에서 실행되기도 하므로, 옆에 있는 같은 이름의 파일을 실행하지 않고 내장된 설정 창만 씀
const SIBLING_ALLOWED: bool = cfg!(any(debug_assertions, feature = "mock"));

fn program(host: &Path) -> io::Result<SettingsProgram> {
    let mut exe = File::open(host)?;
    let Some(size) = packed_size(&mut exe)? else {
        // Cargo builds both binaries side by side for local development.
        let sibling = host.with_file_name(SETTINGS_EXE);
        if SIBLING_ALLOWED && sibling.is_file() {
            return Ok(SettingsProgram::new(sibling));
        }
        return Err(io::Error::new(io::ErrorKind::NotFound, if SIBLING_ALLOWED {
            usagi_core::tr!(
                "설정 실행 파일 없음. 배포 파일을 다시 받거나 usagi-settings를 함께 빌드",
                "Settings executable missing. Download usAgI again or build usagi-settings alongside it"
            )
        } else {
            usagi_core::tr!(
                "설정 창이 들어 있지 않은 실행 파일. 배포 파일을 다시 받거나 scripts\\package-portable.ps1로 묶기",
                "This executable has no settings window. Download usAgI again or bundle it with scripts\\package-portable.ps1"
            )
        }));
    };
    let directory = tempfile::Builder::new().prefix("usagi-settings-").tempdir()?;
    let path = directory.path().join(SETTINGS_EXE);
    let mut output = File::create(&path)?;
    let mut ui = GzDecoder::new(exe.take(size)).take(MAX_UI_SIZE + 1);
    let bytes = io::copy(&mut ui, &mut output)?;
    if bytes == 0 || bytes > MAX_UI_SIZE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid settings executable size"));
    }
    drop(output);
    Ok(SettingsProgram::temporary(path, directory))
}

fn packed_size(file: &mut (impl Read + Seek)) -> io::Result<Option<u64>> {
    let len = file.seek(SeekFrom::End(0))?;
    if len < FOOTER_SIZE {
        return Ok(None);
    }
    let end = len - FOOTER_SIZE;
    file.seek(SeekFrom::Start(end))?;
    let mut size = [0; 8];
    let mut marker = [0; MARKER.len()];
    file.read_exact(&mut size)?;
    file.read_exact(&mut marker)?;
    if &marker != MARKER {
        return Ok(None);
    }
    let size = u64::from_le_bytes(size);
    if size == 0 || size >= end || size > MAX_UI_SIZE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid embedded settings payload"));
    }
    file.seek(SeekFrom::Start(end - size))?;
    Ok(Some(size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn locates_the_embedded_settings_after_the_host_image() {
        let mut bytes = b"host-ui".to_vec();
        bytes.extend_from_slice(&2u64.to_le_bytes());
        bytes.extend_from_slice(MARKER);
        let mut image = Cursor::new(bytes);
        assert_eq!(packed_size(&mut image).unwrap(), Some(2));
        assert_eq!(image.position(), 5);
        let mut payload = [0; 2];
        image.read_exact(&mut payload).unwrap();
        assert_eq!(&payload, b"ui");
    }

    #[test]
    fn distinguishes_development_builds_from_invalid_packages() {
        assert_eq!(packed_size(&mut Cursor::new(b"bare host")).unwrap(), None);
        let mut invalid = b"host".to_vec();
        invalid.extend_from_slice(&10u64.to_le_bytes());
        invalid.extend_from_slice(MARKER);
        assert_eq!(
            packed_size(&mut Cursor::new(invalid)).unwrap_err().kind(),
            io::ErrorKind::InvalidData,
        );
    }
}
