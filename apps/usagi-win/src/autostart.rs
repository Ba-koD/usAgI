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
    let Ok(file) = write_task_file(&task_xml(exe, &user)) else { return false };
    schtasks(&["/Create", "/TN", TASK, "/XML", &file.to_string_lossy(), "/F"])
    // file이 사라지면서 임시 파일도 지움
}

/// schtasks /XML이 읽을 UTF-16 파일. 임시 폴더에 임의 이름으로 새로 만듦: 미리 만들어 둔 같은 이름의 파일·링크를 열지 않음
fn write_task_file(xml: &str) -> std::io::Result<tempfile::TempPath> {
    use std::io::Write;
    let mut bytes = vec![0xFF, 0xFE];
    for u in xml.encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    let mut file = tempfile::Builder::new().prefix("usagi-task-").suffix(".xml").tempfile()?;
    file.write_all(&bytes)?;
    file.flush()?;
    // 핸들을 닫아 schtasks가 읽을 수 있게 (경로는 남고, 값이 사라질 때 지움)
    Ok(file.into_temp_path())
}

/// 작업 스케줄러에 등록된 usAgI 작업이 실행하는 파일 (작업이 없거나 읽을 수 없으면 None)
fn task_target() -> Option<PathBuf> {
    use windows::core::{Interface, BSTR};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
    use windows::Win32::System::TaskScheduler::{IExecAction, ITaskService, TaskScheduler, TASK_ACTION_EXEC};
    use windows::Win32::System::Variant::VARIANT;
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let target = (|| -> windows::core::Result<Option<PathBuf>> {
            let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
            service.Connect(&VARIANT::default(), &VARIANT::default(), &VARIANT::default(), &VARIANT::default())?;
            let task = service.GetFolder(&BSTR::from("\\"))?.GetTask(&BSTR::from(TASK))?;
            let actions = task.Definition()?.Actions()?;
            let mut count = 0;
            actions.Count(&mut count)?;
            for index in 1..=count {
                let action = actions.get_Item(index)?;
                let mut kind = Default::default();
                action.Type(&mut kind)?;
                if kind == TASK_ACTION_EXEC {
                    let mut path = BSTR::new();
                    action.cast::<IExecAction>()?.Path(&mut path)?;
                    let path = path.to_string();
                    let path = path.trim().trim_matches('"');
                    return Ok((!path.is_empty()).then(|| PathBuf::from(path)));
                }
            }
            Ok(None)
        })()
        .ok()
        .flatten();
        if initialized {
            CoUninitialize();
        }
        target
    }
}

/// 대상 exe가 없어진 등록(로그온 작업·Run 값)을 지움. 그 경로에 나중에 다른 파일이 생기면 로그인할 때 실행되므로
fn remove_missing() {
    if task_target().is_some_and(|target| !target.is_file()) {
        schtasks(&["/Delete", "/TN", TASK, "/F"]);
    }
    if win::reg_string(RUN_KEY, RUN_VALUE).and_then(|value| run_value_exe(&value)).is_some_and(|target| !target.is_file()) {
        win::reg_delete(RUN_KEY, RUN_VALUE);
    }
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
/// 내려받기·임시 폴더에서 실행 중이면 켜지 않고 이유를 돌려줌 (그 경로의 파일은 지워지거나 바뀌기 쉬움)
pub fn set(on: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    if let Some(kind) = usagi_core::location::transient(&exe).filter(|_| on) {
        return Err(kind.blocked(("시작 시 실행", "start with Windows")));
    }
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

/// 앱이 뜰 때 (백그라운드): 대상 exe가 없어진 등록을 지우고, 이 exe를 가리키는 예전 Run 등록을 로그온 작업으로 옮김.
/// 다른 위치에 있는 usAgI(설치본·포터블·개발 빌드)를 가리키는 등록은 건드리지 않음.
/// 내려받기·임시 폴더에서 실행 중이면 옮기지 않음 ([`set`]이 거절).
/// 실행 중인 작업을 다시 등록해도 지금 프로세스는 그대로 (확인함).
pub fn refresh() {
    let _ = std::thread::Builder::new().name("usagi-autostart".into()).spawn(|| {
        remove_missing();
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
    fn task_file_gets_a_fresh_random_name_and_is_removed() {
        let first = write_task_file("<Task/>").unwrap();
        let second = write_task_file("<Task/>").unwrap();
        assert_ne!(first.to_path_buf(), second.to_path_buf(), "실행마다 다른 이름");
        let name = first.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("usagi-task-") && name.ends_with(".xml") && name != format!("usagi-task-{}.xml", std::process::id()), "{name}");
        let bytes = std::fs::read(&first).unwrap();
        assert_eq!(&bytes[..4], &[0xFF, 0xFE, b'<', 0], "UTF-16 LE BOM");
        let path = first.to_path_buf();
        drop(first);
        assert!(!path.exists(), "끝나면 지움");
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
