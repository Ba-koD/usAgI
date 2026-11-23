//! Windows 로그인할 때 자동 실행: 작업 스케줄러의 "로그온 시" 작업.
//!
//! 시작 프로그램(레지스트리 Run)은 Windows가 등록 순서대로 하나씩 띄워서, 시작 프로그램이 많으면
//! 로그인 후 1분 넘게 늦게 뜸. 로그온 작업은 그 대기열을 거치지 않고 로그인 직후 실행.
//! 관리자 권한 없이 현재 사용자 작업으로 등록. 작업을 만들 수 없으면 Run 등록으로 대체.
//!
//! 자동 실행 명령에는 `--autostart`를 붙여서, 이미 떠 있을 때 설정 창을 열지 않고 조용히 끝냄.

use crate::win;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};

/// 자동 실행으로 시작됐다는 표시 (중복 실행이면 조용히 끝냄)
pub const ARG: &str = "--autostart";
const TASK: &str = "usAgI";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "usAgI";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 0 = 아직 모름, 1 = 꺼짐, 2 = 켜짐 (설정 화면이 열릴 때마다 schtasks를 부르지 않게)
static STATE: AtomicU8 = AtomicU8::new(0);

fn schtasks(args: &[&str]) -> bool {
    let exe = std::env::var_os("SystemRoot")
        .map(|r| PathBuf::from(r).join("System32").join("schtasks.exe"))
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\schtasks.exe"));
    Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .is_ok_and(|s| s.success())
}

fn task_exists() -> bool {
    schtasks(&["/Query", "/TN", TASK])
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// 현재 사용자 계정 ("도메인\사용자")
fn current_user() -> Option<String> {
    let user = std::env::var("USERNAME").ok().filter(|u| !u.is_empty())?;
    Some(match std::env::var("USERDOMAIN").ok().filter(|d| !d.is_empty()) {
        Some(d) => format!("{d}\\{user}"),
        None => user,
    })
}

/// 로그온 작업 XML. 우선순위 4(보통): 기본값 7은 프로세스를 "낮음" 우선순위로 띄움. 실행 시간 제한 없음.
fn task_xml(exe: &Path, user: &str) -> String {
    let user = xml_escape(user);
    let exe = xml_escape(&exe.to_string_lossy());
    // 작업 스케줄러에 보이는 설명 (등록할 때의 언어)
    let desc = xml_escape(usagi_core::tr!("usAgI: 로그인할 때 AI 구독 사용량 표시 시작", "usAgI: show AI subscription usage at sign-in"));
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>{desc}</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>{exe}</Command><Arguments>{ARG}</Arguments></Exec></Actions>
</Task>
"#
    )
}

fn create_task(exe: &Path) -> bool {
    let Some(user) = current_user() else { return false };
    // schtasks /XML은 UTF-16 파일을 읽음
    let mut bytes = vec![0xFF, 0xFE];
    for u in task_xml(exe, &user).encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    let path = std::env::temp_dir().join(format!("usagi-task-{}.xml", std::process::id()));
    if std::fs::write(&path, bytes).is_err() {
        return false;
    }
    let ok = schtasks(&["/Create", "/TN", TASK, "/XML", &path.to_string_lossy(), "/F"]);
    let _ = std::fs::remove_file(&path);
    ok
}

/// 켜져 있는지 (처음 한 번만 확인하고 기억)
pub fn enabled() -> bool {
    match STATE.load(Ordering::Relaxed) {
        2 => true,
        1 => false,
        _ => {
            let on = win::reg_string(RUN_KEY, RUN_VALUE).is_some() || task_exists();
            STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
    }
}

/// 켜기: 로그온 작업 등록 (실패하면 Run 등록). 끄기: 둘 다 지움.
pub fn set(on: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let applied = if on {
        if create_task(&exe) {
            win::reg_delete(RUN_KEY, RUN_VALUE);
            true
        } else {
            win::reg_set_string(RUN_KEY, RUN_VALUE, &format!("\"{}\" {ARG}", exe.display()))
        }
    } else {
        schtasks(&["/Delete", "/TN", TASK, "/F"]);
        win::reg_delete(RUN_KEY, RUN_VALUE);
        !task_exists() && win::reg_string(RUN_KEY, RUN_VALUE).is_none()
    };
    if applied {
        STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
        Ok(())
    } else {
        STATE.store(0, Ordering::Relaxed);
        Err(usagi_core::tr!("시작 시 실행 설정 실패", "Could not change start at login").into())
    }
}

/// Run 값에 적힌 실행 파일 경로 (`"C:\경로\usagi.exe" --autostart` 또는 따옴표 없는 경로)
fn run_value_exe(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    let path = match value.strip_prefix('"') {
        Some(rest) => rest.split('"').next()?,
        None => value.split(" --").next()?.trim(),
    };
    (!path.is_empty()).then(|| PathBuf::from(path))
}

fn same_file(a: &Path, b: &Path) -> bool {
    let a = a.canonicalize().unwrap_or_else(|_| a.to_owned());
    let b = b.canonicalize().unwrap_or_else(|_| b.to_owned());
    a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy())
}

/// 앱이 뜰 때 (백그라운드): 이 exe를 가리키는 예전 Run 등록을 로그온 작업으로 옮김.
/// 다른 위치의 usAgI(설치본·포터블·개발 빌드)를 가리키는 등록은 건드리지 않음.
/// 실행 중인 작업을 다시 등록해도 지금 프로세스는 그대로 (확인함).
pub fn refresh() {
    let _ = std::thread::Builder::new().name("usagi-autostart".into()).spawn(|| {
        STATE.store(0, Ordering::Relaxed);
        let Ok(exe) = std::env::current_exe() else { return };
        let ours = win::reg_string(RUN_KEY, RUN_VALUE)
            .and_then(|v| run_value_exe(&v))
            .is_some_and(|p| same_file(&p, &exe));
        if ours {
            let _ = set(true);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_xml_escapes_and_has_autostart_arg() {
        let xml = task_xml(Path::new(r"C:\Tools & Apps\<us>\usagi.exe"), r"PC\user'1");
        assert!(xml.contains(r"<Command>C:\Tools &amp; Apps\&lt;us&gt;\usagi.exe</Command>"), "{xml}");
        assert!(xml.contains("<Arguments>--autostart</Arguments>"));
        assert!(xml.contains("<UserId>PC\\user&apos;1</UserId>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"), "실행 시간 제한 없음 (기본 72시간이면 앱이 꺼짐)");
        assert!(xml.contains("<RunLevel>LeastPrivilege</RunLevel>"), "관리자 권한 없이");
    }

    #[test]
    fn reads_the_exe_from_old_and_new_run_values() {
        assert_eq!(run_value_exe(r#""C:\Users\me\Downloads\usagi.exe""#), Some(PathBuf::from(r"C:\Users\me\Downloads\usagi.exe")));
        assert_eq!(run_value_exe(r#""C:\Tools\usagi.exe" --autostart"#), Some(PathBuf::from(r"C:\Tools\usagi.exe")));
        assert_eq!(run_value_exe(r"C:\Tools\usagi.exe --autostart"), Some(PathBuf::from(r"C:\Tools\usagi.exe")));
        assert_eq!(run_value_exe("  "), None);
        assert!(same_file(Path::new(r"C:\TOOLS\usagi.exe"), Path::new(r"c:\tools\USAGI.EXE")));
    }
}
