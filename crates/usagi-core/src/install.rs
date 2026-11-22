//! 공식 CLI 자동 설치 — 계정 연결·사용량 확인에 필요한 CLI가 없으면 각 회사의 공식 설치 프로그램 실행.
//!
//! - Claude Code: `https://claude.ai/install.ps1` (Windows) · `https://claude.ai/install.sh` (macOS·Linux)
//! - Codex CLI:   `https://chatgpt.com/codex/install.ps1` · `https://chatgpt.com/codex/install.sh`
//!
//! 두 설치 프로그램 모두 사용자 폴더에만 설치하고 관리자 권한이 필요 없음.
//! 설치된 실행 파일은 [`crate::login::installed_programs`]가 찾음
//! (Claude: `~/.local/bin/claude`, Codex: Windows `%LOCALAPPDATA%\Programs\OpenAI\Codex\bin`, 그 외 `~/.local/bin/codex`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 설치 프로그램이 이보다 오래 걸리면 중단
#[cfg(not(test))]
const TIMEOUT: Duration = Duration::from_secs(600);
/// 실패 뒤 백그라운드 재시도 간격 (사용자가 계정 추가·로그인을 누르면 바로 다시 시도)
const RETRY_AFTER: Duration = Duration::from_secs(30 * 60);

/// 공급자별 마지막 실패 시각. 잠금이 곧 "한 번에 하나의 설치".
static LAST_FAILURE: [Mutex<Option<Instant>>; 2] = [Mutex::new(None), Mutex::new(None)];
static INSTALLING: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];

fn slot(provider: &str) -> Option<usize> {
    match provider {
        "claude" => Some(0),
        "codex" => Some(1),
        _ => None,
    }
}

pub(crate) fn cli_name(provider: &str) -> &'static str {
    if provider == "claude" {
        "Claude Code CLI"
    } else {
        "Codex CLI"
    }
}

/// 지금 설치 프로그램이 돌고 있는지 (화면 표시용)
pub(crate) fn installing(provider: &str) -> bool {
    slot(provider).is_some_and(|i| INSTALLING[i].load(Ordering::Relaxed))
}

#[cfg_attr(test, allow(dead_code))]
fn script_url(provider: &str) -> &'static str {
    match (provider, cfg!(windows)) {
        ("claude", true) => "https://claude.ai/install.ps1",
        ("claude", false) => "https://claude.ai/install.sh",
        (_, true) => "https://chatgpt.com/codex/install.ps1",
        (_, false) => "https://chatgpt.com/codex/install.sh",
    }
}

/// 설치된 CLI 목록. 하나도 없으면 공식 설치 프로그램으로 설치한 뒤 다시 찾음.
/// `manual`: 사용자가 직접 시작한 연결 (최근 실패와 상관없이 다시 시도).
pub(crate) fn ensure(provider: &str, manual: bool) -> Result<Vec<PathBuf>, String> {
    let found = crate::login::installed_programs(provider);
    if !found.is_empty() {
        return Ok(found);
    }
    let Some(i) = slot(provider) else {
        return Err(crate::login::install_hint(provider));
    };
    let mut last_failure = LAST_FAILURE[i].lock().unwrap_or_else(|e| e.into_inner());
    // 기다리는 동안 다른 스레드가 설치를 끝냈을 수 있음
    let found = crate::login::installed_programs(provider);
    if !found.is_empty() {
        return Ok(found);
    }
    if !manual && last_failure.is_some_and(|at| at.elapsed() < RETRY_AFTER) {
        return Err(crate::login::install_hint(provider));
    }
    INSTALLING[i].store(true, Ordering::Relaxed);
    let result = run(provider);
    INSTALLING[i].store(false, Ordering::Relaxed);
    let found = crate::login::installed_programs(provider);
    if !found.is_empty() {
        *last_failure = None;
        return Ok(found);
    }
    *last_failure = Some(Instant::now());
    let reason = result.err().unwrap_or_else(|| crate::tr!("설치 후 실행 파일 미발견", "program not found after install").into());
    let (cli, hint) = (cli_name(provider), crate::login::install_hint(provider));
    Err(crate::tr!(format!("{cli} 자동 설치 실패 ({reason}). {hint}"), format!("Automatic install of {cli} failed ({reason}). {hint}")))
}

/// 테스트는 실제 시스템에 아무것도 설치하지 않음
#[cfg(test)]
fn run(_provider: &str) -> Result<(), String> {
    Err("테스트 환경".into())
}

#[cfg(not(test))]
fn run(provider: &str) -> Result<(), String> {
    use std::io::Read;
    use std::process::Stdio;

    let url = script_url(provider);
    let mut cmd = if cfg!(windows) {
        // PATH의 다른 powershell이 아닌 시스템 Windows PowerShell
        let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let shell = root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        let mut c = crate::login::quiet_command(&shell);
        c.args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command"])
            .arg(format!("$ProgressPreference='SilentlyContinue'; irm {url} | iex"));
        c
    } else {
        // macOS 앱은 터미널 PATH가 없으므로 절대 경로
        let shell = if provider == "claude" { "/bin/bash" } else { "/bin/sh" };
        let mut c = std::process::Command::new(shell);
        // 전용 임시 폴더(mktemp -d, 0700)에 다 받은 뒤 실행. 받다가 끊기면 curl이 실패로 끝나 실행하지 않음. 리디렉션도 https만
        c.arg("-c").arg(format!(
            "set -e; d=$(/usr/bin/mktemp -d); trap '/bin/rm -rf \"$d\"' EXIT; \
             /usr/bin/curl -fsSL --proto '=https' --proto-redir '=https' -o \"$d/install.sh\" {url}; {shell} \"$d/install.sh\""
        ));
        c
    };
    // 기본 위치에 설치 (계정별 프로필 폴더는 설치와 무관), 질문 없이 진행
    cmd.env("CODEX_NON_INTERACTIVE", "1")
        .env_remove("CODEX_HOME")
        .env_remove("CODEX_INSTALL_DIR")
        .env_remove("CLAUDE_CONFIG_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child =
        cmd.spawn().map_err(|e| crate::tr!(format!("설치 프로그램 실행 실패: {e}"), format!("Failed to start the installer: {e}")))?;
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = (&mut pipe).take(256 * 1024).read_to_end(&mut bytes);
            }
            String::from_utf8_lossy(&bytes).into_owned()
        })
    };
    let out = read(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = read(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < TIMEOUT => std::thread::sleep(Duration::from_millis(200)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(crate::tr!("설치 시간 초과", "Install timed out").into());
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    let mut text = err.join().unwrap_or_default();
    text.push('\n');
    text.push_str(&out.join().unwrap_or_default());
    if status.success() {
        Ok(())
    } else {
        Err(last_line(&text).unwrap_or_else(|| {
            let code = status.code().unwrap_or(-1);
            crate::tr!(format!("종료 코드 {code}"), format!("Exit code {code}"))
        }))
    }
}

/// 오류 출력의 마지막 의미 있는 줄 (색 코드 제거, 160자까지)
#[cfg_attr(test, allow(dead_code))]
fn last_line(text: &str) -> Option<String> {
    let line = text.lines().map(strip_ansi).map(|l| l.trim().to_string()).rfind(|l| !l.is_empty())?;
    Some(line.chars().take(160).collect())
}

fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // ESC [ ... 문자 하나로 끝나는 제어 순서 건너뜀
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_only_official_installers() {
        for provider in ["claude", "codex"] {
            let url = script_url(provider);
            assert!(url.starts_with("https://claude.ai/install.") || url.starts_with("https://chatgpt.com/codex/install."));
        }
        assert_eq!(cli_name("claude"), "Claude Code CLI");
        assert!(!installing("claude") && !installing("other"));
    }

    #[test]
    fn reports_the_last_error_line_without_colors() {
        let text = "Downloading...\n\u{1b}[31mChecksum verification failed\u{1b}[0m\n\n";
        assert_eq!(last_line(text).as_deref(), Some("Checksum verification failed"));
        assert_eq!(last_line(" \n "), None);
    }
}
