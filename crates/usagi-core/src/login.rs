//! 계정 로그인 상태 — 각 회사의 공식 CLI로만 확인하고, 로그인도 공식 흐름으로 띄웁니다.
//!
//! usAgI는 토큰·세션을 읽거나 저장하지 않습니다. Anthropic 정책상 서드파티 앱은 Claude.ai 로그인을
//! 대신 받거나 자격 증명을 저장할 수 없고, 로그인은 Anthropic의 흐름에서 끝나야 합니다.
//! (https://code.claude.com/docs/en/legal-and-compliance)
//!
//! - Claude: `claude auth status --text` / `claude auth login`  (프로필: CLAUDE_CONFIG_DIR)
//! - Codex:  `codex login status`        / `codex login`        (프로필: CODEX_HOME)
//!
//! Claude Code 로그인에는 Anthropic이 정한 종료일이 있고 사용해도 연장되지 않습니다.
//! 그래서 usAgI는 상태와 종료일을 보여 주고, 만료되면 알리고, 한 번에 다시 로그인하게 돕습니다.

use crate::accounts::{default_claude_dir, default_codex_dir, Account};
use crate::time;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

static SELECTED: std::sync::LazyLock<Mutex<std::collections::HashMap<String, PathBuf>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoginStatus {
    /// None = 확인 못 함(CLI 없음 등)
    pub logged_in: Option<bool>,
    pub email: Option<String>,
    pub org: Option<String>,
    /// "Claude Max account" 같은 로그인 방식/플랜
    pub plan: Option<String>,
    /// "Sign-in ends" 같은 만료 안내 (CLI가 준 그대로)
    pub ends: Option<String>,
    pub checked_at: f64,
    pub error: Option<String>,
}

pub(crate) fn same_dir(a: &Path, b: Option<PathBuf>) -> bool {
    b.is_some_and(|b| {
        let a = a.canonicalize().unwrap_or_else(|_| a.to_owned());
        let b = b.canonicalize().unwrap_or(b);
        if cfg!(windows) { a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()) } else { a == b }
    })
}

/// 기본 프로필이 아니면 프로필 폴더를 환경 변수로 넘김
pub fn profile_env(acc: &Account) -> Option<(&'static str, String)> {
    let dir = acc.path();
    match acc.provider.as_str() {
        "claude" if !same_dir(&dir, default_claude_dir()) => Some(("CLAUDE_CONFIG_DIR", acc.dir.clone())),
        "codex" if !same_dir(&dir, default_codex_dir()) => Some(("CODEX_HOME", acc.dir.clone())),
        _ => None,
    }
}

pub(crate) fn program_candidates(provider: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let home = crate::logs::home_dir();
    match provider {
        "claude" => {
            out.push(PathBuf::from("claude"));
            if let Some(h) = &home {
                out.push(h.join(".local").join("bin").join(if cfg!(windows) { "claude.exe" } else { "claude" }));
                if !cfg!(windows) {
                    out.push(h.join(".claude").join("local").join("claude"));
                }
            }
            if !cfg!(windows) {
                // macOS 앱은 터미널 PATH를 물려받지 않으므로 흔한 설치 위치도 직접
                out.push(PathBuf::from("/opt/homebrew/bin/claude"));
                out.push(PathBuf::from("/usr/local/bin/claude"));
            }
        }
        _ => {
            out.push(PathBuf::from("codex"));
            if cfg!(windows) {
                if let Some(appdata) = std::env::var_os("APPDATA") {
                    let npm = PathBuf::from(appdata).join("npm");
                    let (arch, triple) = if std::env::consts::ARCH == "aarch64" {
                        ("arm64", "aarch64-pc-windows-msvc")
                    } else {
                        ("x64", "x86_64-pc-windows-msvc")
                    };
                    let modules = npm.join("node_modules").join("@openai");
                    for packages in [modules.join("codex/node_modules/@openai"), modules] {
                        out.push(packages.join(format!("codex-win32-{arch}/vendor/{triple}/bin/codex.exe")));
                    }
                    out.push(npm.join("codex.cmd"));
                }
                out.push(PathBuf::from("codex.cmd"));
                // 공식 설치 프로그램 위치
                if let Some(local) = std::env::var_os("LOCALAPPDATA") {
                    out.push(PathBuf::from(local).join("Programs").join("OpenAI").join("Codex").join("bin").join("codex.exe"));
                }
            }
            if !cfg!(windows) {
                out.push(PathBuf::from("/opt/homebrew/bin/codex"));
                out.push(PathBuf::from("/usr/local/bin/codex"));
                if let Some(h) = &home {
                    out.push(h.join(".local").join("bin").join("codex")); // 공식 설치 프로그램 위치
                    out.push(h.join(".npm-global").join("bin").join("codex"));
                }
            }
        }
    }
    out
}

/// PATH에서 찾기 (경로가 붙은 이름은 그 파일이 있는지만). Windows는 확장자 없는 이름에 .exe를 붙여 찾음 (std::process::Command와 같게).
fn which(name: &Path) -> Option<PathBuf> {
    if name.is_absolute() || name.components().count() > 1 {
        return name.is_file().then(|| name.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| {
        let p = dir.join(name);
        let p = if cfg!(windows) && p.extension().is_none() { p.with_extension("exe") } else { p };
        p.is_file().then_some(p)
    })
}

/// 실제로 설치돼 있는 CLI 실행 파일들 (찾은 순서대로, 중복 없음)
pub(crate) fn installed_programs(provider: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let selected = SELECTED.lock().ok().and_then(|map| map.get(provider).cloned());
    for c in selected.into_iter()
        .chain(program_candidates(provider))
        .chain(crate::desktop::desktop_programs(provider))
        .chain(crate::desktop::previously_installed(provider)) {
        if let Some(p) = which(&c) {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

pub(crate) fn remember_program(provider: &str, path: PathBuf) {
    if let Ok(mut selected) = SELECTED.lock() {
        selected.insert(provider.into(), path);
    }
}

pub(crate) fn quiet_command(program: &Path) -> Command {
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    if crate::desktop::previously_installed("claude").as_deref() == Some(program) {
        cmd.env("DISABLE_UPDATES", "1");
    }
    cmd
}

fn output_with_timeout(mut command: Command, timeout: Duration) -> Result<String, String> {
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|e| e.to_string())?;
    let read = |mut output: Box<dyn std::io::Read + Send>| std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = (&mut output).take(256 * 1024).read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    });
    let out = read(Box::new(child.stdout.take().expect("stdout")));
    let err = read(Box::new(child.stderr.take().expect("stderr")));
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) if out.is_finished() && err.is_finished() => break,
            Ok(_) if start.elapsed() < timeout => std::thread::sleep(Duration::from_millis(50)),
            state => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(state
                    .err()
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| crate::tr!("연결 도구 응답 시간 초과", "The CLI did not respond in time").into()));
            }
        }
    }
    let mut output = out.join().unwrap_or_default();
    output.push_str(&err.join().unwrap_or_default());
    Ok(output)
}

pub(crate) fn usable_program(provider: &str, path: &Path) -> bool {
    let mut cmd = quiet_command(path);
    cmd.arg("--version");
    output_with_timeout(cmd, Duration::from_secs(8)).is_ok_and(|text| match provider {
        "codex" => text.contains("codex-cli "),
        "claude" => text.contains("(Claude Code)"),
        _ => false,
    })
}

/// Ask the official command whether the default profile already has a subscription.
pub(crate) fn has_subscription(provider: &str, path: &Path) -> bool {
    let mut cmd = quiet_command(path);
    let args: &[&str] = if provider == "claude" { &["auth", "status"] } else { &["login", "status"] };
    cmd.args(args);
    if provider == "claude" {
        cmd.env_remove("ANTHROPIC_API_KEY").env_remove("ANTHROPIC_AUTH_TOKEN");
    }
    let Ok(output) = output_with_timeout(cmd, Duration::from_secs(12)) else { return false };
    if provider == "claude" {
        let Ok(status) = serde_json::from_str::<serde_json::Value>(&output) else { return false };
        status["loggedIn"].as_bool() == Some(true) && status["authMethod"].as_str() == Some("claude.ai")
    } else {
        output.lines().any(|line| line.trim().eq_ignore_ascii_case("Logged in using ChatGPT"))
    }
}

/// 공식 CLI가 설치돼 있는지 (계정을 추가하기 전에 확인)
pub fn cli_installed(provider: &str) -> bool {
    !installed_programs(provider).is_empty()
}

/// 자동 설치가 안 될 때 보여 줄 직접 설치 방법 (각 회사의 공식 설치 프로그램)
pub fn install_hint(provider: &str) -> String {
    let (name, script) = match (provider, cfg!(windows)) {
        ("claude", true) => ("Claude Code CLI", "irm https://claude.ai/install.ps1 | iex"),
        ("claude", false) => ("Claude Code CLI", "curl -fsSL https://claude.ai/install.sh | bash"),
        (_, true) => ("Codex CLI", "irm https://chatgpt.com/codex/install.ps1 | iex"),
        (_, false) => ("Codex CLI", "curl -fsSL https://chatgpt.com/codex/install.sh | sh"),
    };
    let shell = if cfg!(windows) { "PowerShell" } else { crate::tr!("터미널", "Terminal") };
    crate::tr!(format!("{name} 직접 설치: {shell}에서 {script}"), format!("Install {name} manually: in {shell}, run {script}"))
}

/// 설치된 CLI 목록. 하나도 없으면 공식 설치 프로그램으로 자동 설치 (실패하면 직접 설치 방법을 오류로).
pub(crate) fn programs_or_hint(provider: &str) -> Result<Vec<PathBuf>, String> {
    crate::install::ensure(provider, false)
}

fn run(acc: &Account, args: &[&str]) -> Result<String, String> {
    let mut last_err = String::from(crate::tr!("CLI 찾기 실패", "CLI not found"));
    for prog in programs_or_hint(&acc.provider)? {
        let mut cmd = quiet_command(&prog);
        cmd.args(args);
        if let Some((k, v)) = profile_env(acc) {
            cmd.env(k, v);
        }
        match output_with_timeout(cmd, Duration::from_secs(15)) {
            Ok(text) => return Ok(text),
            Err(e) => last_err = format!("{}: {e}", prog.display()),
        }
    }
    Err(last_err)
}

/// "Key: Value" 줄 → (key 소문자, value)
fn fields(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))
        .filter(|(k, v)| !k.is_empty() && !v.is_empty() && k.len() < 40)
        .collect()
}

pub fn parse_claude(text: &str) -> LoginStatus {
    let f = fields(text);
    let get = |key: &str| f.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
    let low = text.to_lowercase();
    let expired = get("login").is_some_and(|v| v.to_lowercase().starts_with("expired")) || low.contains("not logged in");
    let logged_in = if expired {
        Some(false)
    } else if ["email", "login method", "login", "sign-in ends"].iter().any(|key| get(key).is_some()) {
        Some(true)
    } else {
        None
    };
    LoginStatus {
        logged_in,
        email: get("email"),
        org: get("organization"),
        plan: get("login method"),
        ends: get("sign-in ends").or_else(|| get("login").filter(|_| !expired)),
        checked_at: time::now(),
        error: None,
    }
}

pub fn parse_codex(text: &str) -> LoginStatus {
    let account = text.lines().find_map(|line| line.trim().strip_prefix("Logged in using "));
    let logged_in = if account.is_some() {
        Some(true)
    } else if text.to_ascii_lowercase().contains("not logged in") {
        Some(false)
    } else {
        None
    };
    LoginStatus {
        logged_in,
        email: None,
        plan: None,
        org: account.map(str::to_owned),
        ends: None,
        checked_at: time::now(),
        error: None,
    }
}

/// 공식 CLI로 로그인 상태 확인 (1초 안팎 걸리므로 백그라운드에서)
pub fn status(acc: &Account) -> LoginStatus {
    let result = match acc.provider.as_str() {
        "claude" => run(acc, &["auth", "status", "--text"]).map(|t| {
            let mut st = parse_claude(&t);
            if st.logged_in == Some(true) {
                if let Some(tier) = claude_tier(acc) {
                    st.plan = Some(tier);
                }
            }
            st
        }),
        "codex" => run(acc, &["login", "status"]).map(|t| parse_codex(&t)),
        _ => return LoginStatus::default(),
    };
    result.unwrap_or_else(|e| LoginStatus { logged_in: None, checked_at: time::now(), error: Some(e), ..Default::default() })
}

/// Claude Code가 저장해 둔 계정 프로필(.claude.json의 oauthAccount, 토큰 아님)에서 요금제 배율.
/// 기본 프로필(~/.claude)은 ~/.claude.json, 다른 프로필은 그 폴더 안의 .claude.json.
pub fn claude_tier(acc: &Account) -> Option<String> {
    let dir = acc.path();
    let mut files = vec![dir.join(".claude.json")];
    if dir.file_name().is_some_and(|n| n == ".claude") {
        if let Some(parent) = dir.parent() {
            files.push(parent.join(".claude.json"));
        }
    }
    files.into_iter().find_map(|p| {
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
        let a = v.get("oauthAccount")?;
        ["userRateLimitTier", "organizationRateLimitTier"]
            .iter()
            .find_map(|k| a.get(*k).and_then(|t| t.as_str()).and_then(tier_label))
    })
}

/// 요금제 이름 + 배율(있으면): default_claude_max_5x → "Max 5x", default_claude_max_20x → "Max 20x",
/// default_claude_pro → "Pro". 처음 보는 이름도 같은 규칙으로 (예: claude_ultra_10x → "Ultra 10x").
pub fn tier_label(tier: &str) -> Option<String> {
    let mut name = Vec::new();
    let mut mult = None;
    for part in tier.split(|c: char| c == '_' || c == '-' || c.is_whitespace()).filter(|p| !p.is_empty()) {
        let low = part.to_lowercase();
        if matches!(low.as_str(), "default" | "claude" | "ai" | "tier" | "plan" | "account") {
            continue;
        }
        if low.len() > 1 && low.ends_with('x') && low[..low.len() - 1].chars().all(|c| c.is_ascii_digit()) {
            mult = Some(low); // 5x, 20x
            continue;
        }
        let mut c = low.chars();
        if let Some(f) = c.next() {
            name.push(f.to_uppercase().collect::<String>() + c.as_str());
        }
    }
    if name.is_empty() {
        return None;
    }
    let name = name.join(" ");
    Some(match mult {
        Some(m) => format!("{name} {m}"),
        None => name,
    })
}

/// 공식 CLI로 로그아웃 (usAgI가 만든 프로필 계정을 지울 때)
pub fn logout(acc: &Account) -> Result<(), String> {
    let args: &[&str] = if acc.is_claude() { &["auth", "logout"] } else { &["logout"] };
    run(acc, args).map(|_| ())
}

/// 공식 로그인 명령 (보이는 콘솔 창에서 실행해야 함: 브라우저 열기/코드 입력)
pub fn login_args(acc: &Account) -> (String, Vec<String>) {
    match acc.provider.as_str() {
        "codex" => ((if cfg!(windows) { "codex.cmd" } else { "codex" }).into(), vec!["login".into()]),
        _ => ("claude".into(), vec!["auth".into(), "login".into()]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_finds_only_real_programs() {
        let shell = if cfg!(windows) { "cmd" } else { "sh" };
        assert!(which(Path::new(shell)).is_some(), "PATH의 셸은 찾아야 함");
        assert!(which(Path::new("usagi-no-such-cli-xyz")).is_none());
        assert!(which(&std::env::temp_dir().join("usagi-no-such-cli-xyz")).is_none(), "없는 절대 경로");
        assert!(install_hint("codex").contains("Codex CLI"));
        assert!(install_hint("claude").contains("Claude Code CLI"));
    }

    #[test]
    fn claude_tier_labels() {
        assert_eq!(tier_label("default_claude_max_5x").as_deref(), Some("Max 5x"));
        assert_eq!(tier_label("default_claude_max_20x").as_deref(), Some("Max 20x"));
        assert_eq!(tier_label("default_claude_pro").as_deref(), Some("Pro"));
        assert_eq!(tier_label("claude_ultra_10x").as_deref(), Some("Ultra 10x"));
        assert_eq!(tier_label("default_claude_ai"), None);
    }

    #[test]
    fn parses_claude_text() {
        let expired = "Login: Expired — log in again\nOrganization: Someone's Organization\nEmail: a@b.c\nNot logged in. Run claude auth login to authenticate.";
        let s = parse_claude(expired);
        assert_eq!(s.logged_in, Some(false));
        assert_eq!(s.email.as_deref(), Some("a@b.c"));
        let ok = "Sign-in ends: Dec 20, 2026\nOrganization: Org\nEmail: a@b.c\n";
        let s = parse_claude(ok);
        assert_eq!(s.logged_in, Some(true));
        assert_eq!(s.ends.as_deref(), Some("Dec 20, 2026"));
    }

    #[test]
    fn parses_codex_text() {
        assert_eq!(parse_codex("Logged in using ChatGPT\n").logged_in, Some(true));
        assert_eq!(parse_codex("Logged in using ChatGPT\n").org.as_deref(), Some("ChatGPT"));
        assert_eq!(parse_codex("Not logged in\n").logged_in, Some(false));
    }
}

// ------------------------------------------------------------------ 창 없는 로그인 세션

use std::io::{Read as _, Write as _};
use std::process::{Child, ChildStdin};
use std::sync::{Arc, Mutex};

/// 설정 화면에 보여 줄 로그인 진행 상황
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionView {
    pub account_id: String,
    /// 브라우저가 안 열렸을 때 열 주소
    pub url: Option<String>,
    /// CLI가 코드 입력을 받을 수 있는 상태 (브라우저에 코드가 보일 때만 필요)
    pub accepts_code: bool,
    /// None = 진행 중, Some(true) = 성공, Some(false) = 실패
    pub done: Option<bool>,
    pub message: Option<String>,
    pub code_sent: bool,
}

/// 공식 로그인 명령을 콘솔 창 없이 실행하고, 출력에서 주소·코드 입력 요청·결과를 읽습니다.
/// 코드가 필요하면 앱의 입력칸에서 받아 표준 입력으로 넘깁니다. 토큰은 CLI가 직접 저장합니다.
pub struct LoginSession {
    pub account_id: String,
    child: Child,
    stdin: Option<ChildStdin>,
    output: Arc<Mutex<String>>,
    view: SessionView,
    started: f64,
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Only expose provider authorization links in the UI.
fn find_url(text: &str) -> Option<String> {
    text.split_whitespace()
        .filter_map(|word| {
            let url = word.trim_end_matches(['.', ',', ')', '"', '\'']);
            let (host, path) = url.strip_prefix("https://")?.split_once('/')?;
            let official = matches!(host, "auth.openai.com" | "claude.ai" | "claude.com" | "platform.claude.com" | "console.anthropic.com");
            (official && path.contains("oauth")).then(|| url.to_owned())
        })
        .last()
}

fn spawn_reader(pipe: Box<dyn std::io::Read + Send>, out: Arc<Mutex<String>>) {
    std::thread::spawn(move || {
        let mut pipe = pipe;
        let mut buf = [0u8; 1024];
        while let Ok(n) = pipe.read(&mut buf) {
            if n == 0 {
                break;
            }
            if let Ok(mut o) = out.lock() {
                o.push_str(&String::from_utf8_lossy(&buf[..n]));
                // 로그인 주소와 결과는 끝부분에 나오므로 뒤쪽만 남김 (출력이 끝없이 쌓이지 않게)
                if o.len() > 64 * 1024 {
                    let mut cut = o.len() - 32 * 1024;
                    while !o.is_char_boundary(cut) {
                        cut += 1;
                    }
                    o.drain(..cut);
                }
            }
        }
    });
}

impl LoginSession {
    pub fn start(acc: &Account) -> Result<Self, String> {
        let mut last_err = String::from(crate::tr!("CLI 찾기 실패", "CLI not found"));
        for prog in programs_or_hint(&acc.provider)? {
            match Self::start_with(acc, &prog) {
                Ok(session) => return Ok(session),
                Err(error) => last_err = error,
            }
        }
        Err(last_err)
    }

    pub(crate) fn start_with(acc: &Account, prog: &Path) -> Result<Self, String> {
        let (_, args) = login_args(acc);
        let mut cmd = quiet_command(prog);
        cmd.args(&args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Some((k, v)) = profile_env(acc) {
            cmd.env(k, v);
        }
        match cmd.spawn() {
            Ok(mut child) => {
                let output = Arc::new(Mutex::new(String::new()));
                if let Some(p) = child.stdout.take() {
                    spawn_reader(Box::new(p), output.clone());
                }
                if let Some(p) = child.stderr.take() {
                    spawn_reader(Box::new(p), output.clone());
                }
                let stdin = child.stdin.take();
                Ok(Self {
                    account_id: acc.id.clone(),
                    child,
                    stdin,
                    output,
                    view: SessionView { account_id: acc.id.clone(), ..Default::default() },
                    started: time::now(),
                })
            }
            Err(e) => Err(format!("{}: {e}", prog.display())),
        }
    }


    /// 출력과 프로세스 상태를 읽어 진행 상황 갱신
    pub fn poll(&mut self) -> &SessionView {
        if self.view.done.is_some() {
            return &self.view;
        }
        let text = self.output.lock().map(|o| strip_ansi(&o)).unwrap_or_default();
        let low = text.to_lowercase();
        if let Some(u) = find_url(&text) {
            self.view.url = Some(u);
        }
        self.view.accepts_code =
            low.contains("paste code") || low.contains("enter code") || low.contains("authorization code") || low.contains("paste the code");
        match self.child.try_wait() {
            Ok(Some(status)) => {
                let said_ok = low.contains("login successful") || low.contains("successfully logged in");
                let ok = status.success() && (said_ok || (!low.contains("error") && !low.contains("failed")));
                self.view.done = Some(ok);
                if !ok {
                    let last = text.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("").to_string();
                    self.view.message = Some(if last.is_empty() {
                        crate::tr!(format!("종료 코드 {:?}", status.code()), format!("Exit code {:?}", status.code()))
                    } else {
                        last
                    });
                }
                self.stdin = None;
            }
            Ok(None) if time::now() - self.started > 600.0 => {
                self.cancel();
                self.view.message =
                    Some(crate::tr!("10분 동안 로그인이 끝나지 않아 취소", "Sign-in canceled after 10 minutes without finishing").into());
            }
            _ => {}
        }
        &self.view
    }

    pub fn view(&self) -> &SessionView {
        &self.view
    }

    /// 브라우저에 보인 코드를 CLI에 넘김
    pub fn submit_code(&mut self, code: &str) -> Result<(), String> {
        let code = code.trim();
        if code.is_empty() {
            return Err(crate::tr!("코드 입력 필요", "Enter the code").into());
        }
        let stdin = self.stdin.as_mut().ok_or(crate::tr!("이미 끝난 로그인", "Sign-in already finished"))?;
        stdin
            .write_all(format!("{code}\n").as_bytes())
            .and_then(|_| stdin.flush())
            .map_err(|e| e.to_string())?;
        self.view.code_sent = true;
        Ok(())
    }

    pub fn cancel(&mut self) {
        if self.view.done.is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.view.done = Some(false);
            self.view.message = Some(crate::tr!("취소", "Canceled").into());
        }
        self.stdin = None;
    }
}

impl Drop for LoginSession {
    fn drop(&mut self) {
        if self.view.done.is_none() {
            self.cancel();
        }
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;

    #[test]
    fn finds_login_url_and_strips_ansi() {
        let out = "\u{1b}[2KOpening browser to sign in…\nIf the browser didn't open, visit: https://claude.com/cai/oauth/authorize?code=true&x=1\nPaste code here if prompted > ";
        let clean = strip_ansi(out);
        assert!(clean.starts_with("Opening"));
        assert_eq!(find_url(&clean).as_deref(), Some("https://claude.com/cai/oauth/authorize?code=true&x=1"));
        let codex = "Starting local login server on http://localhost:1455.\nnavigate to this URL to authenticate:\n\nhttps://auth.openai.com/oauth/authorize?a=b\n";
        assert_eq!(find_url(codex).as_deref(), Some("https://auth.openai.com/oauth/authorize?a=b"));
    }

    #[test]
    fn parses_login_method_as_plan() {
        let s = parse_claude("Login method: Claude Max account\nOrganization: Org\nEmail: a@b.c\n");
        assert_eq!(s.logged_in, Some(true));
        assert_eq!(s.plan.as_deref(), Some("Claude Max account"));
    }
}
