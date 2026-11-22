//! usAgI for macOS — 메뉴 막대에 사용량 (RunCat처럼), 설정 창은 Windows와 같은 화면.

#[cfg(target_os = "macos")]
mod app;
#[cfg(target_os = "macos")]
mod settings;
#[cfg(target_os = "macos")]
mod title;
#[cfg(target_os = "macos")]
mod updater;

fn main() {
    #[cfg(target_os = "macos")]
    {
        let args: Vec<String> = std::env::args().collect();
        match args.get(1).map(String::as_str) {
            // Claude Code 상태 표시줄 (실시간 연결)
            Some("statusline") => statusline(&args),
            // 빌드 스크립트용: 앱 아이콘(.iconset) PNG 만들기
            Some("--iconset") => app::write_iconset(std::path::Path::new(args.get(2).map(String::as_str).unwrap_or("usAgI.iconset"))),
            _ => {
                std::panic::set_hook(Box::new(|info| app::log(&format!("panic: {info}"))));
                app::run();
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    eprintln!("usagi-mac은 macOS 전용. Windows는 usagi-win 사용.");
}

/// Claude Code가 부르는 상태 표시줄 명령. 사용량을 기록하고, 원래 쓰던 상태 표시줄이 있으면 그 출력을 그대로 보여 줌.
#[cfg(target_os = "macos")]
fn statusline(args: &[String]) {
    use std::io::{Read, Write};
    let account = args
        .iter()
        .position(|a| a == "--account")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "claude-1".into());
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let line = usagi_core::bridge::run_statusline(&input, &account);
    let chain = usagi_core::accounts::load(&usagi_core::config::State::load())
        .into_iter()
        .find(|a| a.id == account)
        .and_then(|a| a.chain);
    let out = chain.and_then(|cmd| run_chain(&cmd, &input)).unwrap_or(line);
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(out.as_bytes());
    let _ = stdout.flush();
}

#[cfg(target_os = "macos")]
fn run_chain(cmd: &str, input: &str) -> Option<String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("/bin/sh").arg("-c").arg(cmd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    child.wait_with_output().ok().map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
}
