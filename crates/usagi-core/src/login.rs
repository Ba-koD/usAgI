//! 계정 로그인 상태 — 유효한 로컬 파일을 먼저 확인하고, 필요하면 공식 CLI를 실행합니다.
//!
//! 로그인과 토큰 파일 갱신은 공식 CLI가 담당합니다.
//! 파일에 저장된 세션을 이용한 사용량 조회는 공급자별 모듈에서 합니다.
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
    pub(crate) organization_id: Option<String>,
    pub(crate) credential_fingerprint: Option<u64>,
    /// "Claude Max account" 같은 로그인 방식/플랜
    pub plan: Option<String>,
    /// "Sign-in ends" 같은 만료 안내 (CLI가 준 그대로)
    pub ends: Option<String>,
    pub(crate) sign_in_expiry: Option<SignInExpiry>,
    pub checked_at: f64,
    pub error: Option<String>,
    /// 주 폴더 로그인이 끊겨 같은 계정의 다른 폴더를 대신 쓰는 중이면 그 폴더
    pub fallback: Option<String>,
}

pub(crate) const SIGN_IN_WARNING_DAYS: i32 = 3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SignInExpiry {
    pub expires_at: f64,
    /// The CLI may give an earlier date at which it recommends signing in again.
    reconnect_by: f64,
}

impl SignInExpiry {
    fn at(expires_at: f64) -> Option<Self> {
        (expires_at.is_finite() && expires_at > 0.0)
            .then_some(Self { expires_at, reconnect_by: expires_at })
    }

    fn from_cli(text: &str) -> Option<Self> {
        let timestamp = text.rsplit_once('(')
            .and_then(|(_, suffix)| suffix.strip_suffix(')'))
            .unwrap_or(text)
            .parse::<jiff::Timestamp>()
            .ok()?;
        let mut expiry = Self::at(timestamp.as_second() as f64)?;
        if let Some(date) = text.strip_prefix("between ").and_then(|range| range.split_once(" and "))
            .and_then(|(early, _)| early.parse::<jiff::civil::Date>().ok())
            .and_then(|date| time::local_to_ts(date.year().into(), date.month() as u32, date.day() as u32, 0, 0))
        {
            expiry.reconnect_by = date.min(expiry.expires_at);
        }
        Some(expiry)
    }

    pub(crate) fn needs_reconnect(self, now: f64) -> bool {
        now >= self.reconnect_by - f64::from(SIGN_IN_WARNING_DAYS) * time::DAY
    }

    pub(crate) fn warning_due(self, now: f64) -> bool {
        self.needs_reconnect(now) && now < self.expires_at
    }
}

/// Stable account identifiers, never a credential or a token fingerprint.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub(crate) struct Identity {
    email: Option<String>,
    organization: Option<String>,
    organization_id: Option<String>,
}

impl Identity {
    pub(crate) fn from_status(status: &LoginStatus) -> Option<Self> {
        let identity = Self {
            email: status.email.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned),
            organization: status.org.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned),
            organization_id: status.organization_id.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned),
        };
        (identity.email.is_some() || identity.organization_id.is_some()).then_some(identity)
    }

    pub(crate) fn from_codex(usage: &crate::codex::CodexSnapshot) -> Option<Self> {
        Self::from_status(&LoginStatus {
            email: usage.email.clone(),
            organization_id: usage.workspace_id.clone(),
            ..Default::default()
        })
    }

    pub(crate) fn conflicts(&self, actual: &Self) -> bool {
        fn different(old: &Option<String>, new: &Option<String>) -> bool {
            matches!((old, new), (Some(old), Some(new)) if !old.trim().eq_ignore_ascii_case(new.trim()))
        }
        different(&self.email, &actual.email)
            || match (&self.organization_id, &actual.organization_id) {
                (Some(_), Some(_)) => different(&self.organization_id, &actual.organization_id),
                _ => different(&self.organization, &actual.organization),
            }
    }
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

#[cfg(windows)]
struct ChildJob(windows_sys::Win32::Foundation::HANDLE);

// The parent controls this handle, and never shares it with the child processes.
#[cfg(windows)]
unsafe impl Send for ChildJob {}

#[cfg(windows)]
impl ChildJob {
    fn attach(child: &Child) -> std::io::Result<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::*;
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() { return Err(std::io::Error::last_os_error()); }
            let job = Self(job);
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(job.0, JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32) == 0
                || AssignProcessToJobObject(job.0, child.as_raw_handle()) == 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(job)
        }
    }
}

#[cfg(windows)]
impl Drop for ChildJob {
    fn drop(&mut self) {
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0); }
    }
}

/// Run the original status line with a fixed deadline and bounded output.
pub fn run_statusline_chain(mut command: Command, input: &str) -> Option<String> {
    use std::io::{Read as _, Write as _};
    let mut child = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    #[cfg(windows)]
    let _job = ChildJob::attach(&child).ok();
    let mut stdin = child.stdin.take()?;
    let mut stdout = child.stdout.take()?;
    let input = input.to_owned();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        (&mut stdout).take(64 * 1024 + 1).read_to_end(&mut bytes).ok()?;
        (bytes.len() <= 64 * 1024).then(|| String::from_utf8_lossy(&bytes).into_owned())
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if writer.is_finished() && reader.is_finished() => {
                return status.success().then(|| reader.join().ok().flatten()).flatten();
            }
            Ok(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(15)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn output_with_timeout(mut command: Command, timeout: Duration) -> Result<String, String> {
    let child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|e| e.to_string())?;
    let (mut stdout, stderr) = capture_child_output(child, timeout).map_err(|error| {
        if error.kind() == std::io::ErrorKind::TimedOut {
            crate::tr!("CLI 응답 시간 초과", "CLI response timed out").into()
        } else {
            error.to_string()
        }
    })?;
    stdout.push_str(&stderr);
    Ok(stdout)
}

pub(crate) fn capture_child_output(mut child: Child, timeout: Duration) -> std::io::Result<(String, String)> {
    let read = |mut output: Box<dyn std::io::Read + Send>| std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = (&mut output).take(256 * 1024).read_to_end(&mut bytes);
        // Keep draining after the capture limit so the child never blocks on a full pipe.
        let _ = std::io::copy(&mut output, &mut std::io::sink());
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
                return Err(state.err().unwrap_or_else(|| std::io::ErrorKind::TimedOut.into()));
            }
        }
    }
    Ok((out.join().unwrap_or_default(), err.join().unwrap_or_default()))
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

pub(crate) fn has_file_session(provider: &str, dir: &Path) -> bool {
    file_status(provider, dir).is_some_and(|status| status.logged_in == Some(true))
}

pub(crate) fn file_status(provider: &str, dir: &Path) -> Option<LoginStatus> {
    match provider {
        "claude" => claude_file_status(dir, time::now()),
        "codex" => crate::codex::file_login_status(dir),
        _ => None,
    }
}

/// 공식 CLI가 설치돼 있는지 (계정을 추가하기 전에 확인)
pub fn cli_installed(provider: &str) -> bool {
    !installed_programs(provider).is_empty()
}

pub(crate) fn cli_name(provider: &str) -> &'static str {
    if provider == "claude" { "Claude Code CLI" } else { "Codex CLI" }
}

/// CLI가 없을 때 보여 줄 직접 설치 방법 (각 회사의 공식 설치 프로그램)
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

/// 설치된 CLI 목록. 없으면 직접 설치 방법을 표시합니다.
pub(crate) fn programs_or_hint(provider: &str) -> Result<Vec<PathBuf>, String> {
    let programs = installed_programs(provider);
    if programs.is_empty() { Err(install_hint(provider)) } else { Ok(programs) }
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
            Err(e) => last_err = format!("{e} ({})", prog.display()),
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
    let api_key = get("login method").is_some_and(|method| method.to_ascii_lowercase().contains("api key"));
    let logged_in = if expired || api_key {
        Some(false)
    } else if ["email", "login method", "login", "sign-in ends", "sign-in"].iter().any(|key| get(key).is_some()) {
        Some(true)
    } else {
        None
    };
    let sign_in_ends = get("sign-in ends").or_else(|| get("sign-in"));
    let sign_in_expiry = sign_in_ends.as_deref().and_then(SignInExpiry::from_cli);
    LoginStatus {
        logged_in,
        email: get("email"),
        org: get("organization"),
        plan: get("login method"),
        ends: sign_in_ends.or_else(|| get("login").filter(|_| !expired)),
        sign_in_expiry,
        checked_at: time::now(),
        organization_id: get("organization id"),
        ..Default::default()
    }
}

pub fn parse_codex(text: &str) -> LoginStatus {
    let account = text.lines().find_map(|line| line.trim().strip_prefix("Logged in using "));
    let logged_in = if account.is_some_and(|value| value.eq_ignore_ascii_case("ChatGPT")) {
        Some(true)
    } else if account.is_some() || text.to_ascii_lowercase().contains("not logged in") {
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
        ..Default::default()
    }
}

/// 로컬 로그인 상태 확인. 필요한 경우 공식 CLI를 실행하므로 백그라운드에서 호출.
pub fn status(acc: &Account) -> LoginStatus {
    let result = match acc.provider.as_str() {
        "claude" => match claude_file_status(&acc.path(), time::now()) {
            Some(status) => Ok(status),
            None => run(acc, &["auth", "status", "--text"]).map(|t| {
                let mut st = parse_claude(&t);
                if st.organization_id.is_none() {
                    if let (Some(email), Some(org)) = (&st.email, &st.org) {
                        st.organization_id = claude_profiles(&acc.path()).find(|profile| {
                            profile.email_address.as_deref().is_some_and(|v| v.trim().eq_ignore_ascii_case(email.trim()))
                                && profile.organization_name.as_deref().is_some_and(|v| v.trim().eq_ignore_ascii_case(org.trim()))
                        }).and_then(|profile| profile.organization_uuid);
                    }
                }
                if st.logged_in == Some(true) {
                    if let Some(tier) = claude_tier(acc) {
                        st.plan = Some(tier);
                    }
                }
                st
            }),
        },
        "codex" => match crate::codex::file_login_status(&acc.path()) {
            Some(status) => Ok(status),
            None => run(acc, &["login", "status"]).map(|t| parse_codex(&t)),
        },
        _ => return LoginStatus::default(),
    };
    result.unwrap_or_else(|e| LoginStatus { logged_in: None, checked_at: time::now(), error: Some(e), ..Default::default() })
}

#[derive(Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeProfile {
    email_address: Option<String>,
    organization_name: Option<String>,
    organization_uuid: Option<String>,
    user_rate_limit_tier: Option<String>,
    organization_rate_limit_tier: Option<String>,
}

impl ClaudeProfile {
    fn tier(&self) -> Option<String> {
        self.user_rate_limit_tier.as_deref().and_then(tier_label)
            .or_else(|| self.organization_rate_limit_tier.as_deref().and_then(tier_label))
    }
}

pub fn claude_tier(acc: &Account) -> Option<String> {
    claude_profiles(&acc.path()).find_map(|profile| profile.tier())
}

/// 기본 프로필(~/.claude)은 ~/.claude.json에 계정 정보가 있을 수 있습니다.
fn claude_profiles(dir: &Path) -> impl Iterator<Item = ClaudeProfile> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Settings {
        oauth_account: Option<ClaudeProfile>,
    }

    let parent = dir.file_name().filter(|n| *n == ".claude")
        .and_then(|_| dir.parent()).map(|dir| dir.join(".claude.json"));
    std::iter::once(dir.join(".claude.json")).chain(parent).filter_map(|path| {
        let file = std::fs::File::open(path).ok()?;
        let settings: Settings = serde_json::from_reader(std::io::BufReader::new(file)).ok()?;
        settings.oauth_account
    })
}

fn claude_file_status(dir: &Path, now: f64) -> Option<LoginStatus> {
    let session = crate::claude_oauth::file_session(dir, now)?;
    let mut profiles = claude_profiles(dir);
    let profile = profiles.next().unwrap_or_default();
    let plan = profile.tier()
        .or_else(|| profiles.find_map(|profile| profile.tier()))
        .or_else(|| session.rate_limit_tier.as_deref().and_then(tier_label))
        .or_else(|| session.subscription_type.as_deref().and_then(tier_label));
    let sign_in_expiry = session.refresh_token_expires_at.and_then(|ms| SignInExpiry::at(ms / 1000.0));
    let ends = sign_in_expiry
        .map(|expiry| {
            let when = crate::texts::fmt_clock(expiry.expires_at, now);
            crate::tr!(format!("로그인 만료 {when}"), format!("Sign-in ends {when}"))
        });
    Some(LoginStatus {
        logged_in: Some(true),
        email: profile.email_address.filter(|s| !s.trim().is_empty()),
        org: profile.organization_name.filter(|s| !s.trim().is_empty()),
        organization_id: profile.organization_uuid.filter(|s| !s.trim().is_empty()),
        credential_fingerprint: Some(session.fingerprint()),
        plan,
        ends,
        sign_in_expiry,
        checked_at: now,
        ..Default::default()
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

/// 공식 로그인 명령. CLI가 기본 브라우저에서 계정 승인을 요청합니다.
pub fn login_args(acc: &Account) -> (String, Vec<String>) {
    match acc.provider.as_str() {
        "codex" => ((if cfg!(windows) { "codex.cmd" } else { "codex" }).into(), vec!["login".into()]),
        _ => {
            let mut args = vec!["auth".into(), "login".into(), "--claudeai".into()];
            if let Some(email) = acc.identity.as_ref().and_then(|identity| identity.email.as_ref()) {
                args.extend(["--email".into(), email.clone()]);
            }
            ("claude".into(), args)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_capture_drains_stderr_and_caps_captured_output() {
        const CHILD: &str = "USAGI_TEST_CAPTURE_CHILD";
        if std::env::var_os(CHILD).is_some() {
            std::io::stderr().write_all(&vec![b'x'; 320 * 1024]).unwrap();
            std::io::stdout().write_all(b"capture completed\n").unwrap();
            return;
        }
        let child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "login::tests::cli_capture_drains_stderr_and_caps_captured_output", "--nocapture"])
            .env(CHILD, "1")
            .stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().unwrap();
        let (stdout, stderr) = capture_child_output(child, Duration::from_secs(10)).unwrap();
        assert!(stdout.contains("capture completed"));
        assert_eq!(stderr.len(), 256 * 1024);
    }

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
    fn reads_claude_profile_metadata_from_its_home_layout() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".claude");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(home.path().join(".claude.json"), r#"{
            "oauthAccount": {
                "emailAddress": "test@example.com",
                "organizationName": "Test org",
                "organizationRateLimitTier": "default_claude_max_5x"
            }
        }"#).unwrap();

        let profile = claude_profiles(&dir).next().unwrap();
        assert_eq!(profile.tier().as_deref(), Some("Max 5x"));
        assert_eq!(profile.email_address.as_deref(), Some("test@example.com"));
        assert_eq!(profile.organization_name.as_deref(), Some("Test org"));
        assert!(claude_profiles(&home.path().join("other-profile")).next().is_none());
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
        assert!(s.sign_in_expiry.is_none());
    }

    #[test]
    fn parses_claude_sign_in_range_and_uses_the_earlier_date_for_warning() {
        let status = parse_claude("Sign-in ends: between 2026-11-20 and 2026-11-23 (2026-11-23T17:04:11.000Z)\n");
        let expiry = status.sign_in_expiry.unwrap();
        let expected_end = time::parse_iso("2026-11-23T17:04:11Z").unwrap();
        let first_date = time::local_to_ts(2026, 11, 20, 0, 0).unwrap();
        let first_warning = first_date - 3.0 * time::DAY;
        assert_eq!(expiry.expires_at, expected_end);
        assert!(!expiry.warning_due(first_warning - 1.0));
        assert!(expiry.warning_due(first_warning));
        assert!(!expiry.warning_due(expected_end));
    }

    #[test]
    fn file_sign_in_expiry_is_distinct_from_access_token_expiry() {
        let profile = tempfile::tempdir().unwrap();
        std::fs::write(profile.path().join(".credentials.json"), serde_json::json!({
            "claudeAiOauth": {
                "accessToken": "sk-ant-oat01-fixture",
                "expiresAt": 500_000,
                "refreshTokenExpiresAt": 900_000_000,
                "scopes": ["user:profile"]
            }
        }).to_string()).unwrap();
        let status = claude_file_status(profile.path(), 100.0).unwrap();
        assert_eq!(status.sign_in_expiry.unwrap().expires_at, 900_000.0);
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
    #[cfg(windows)]
    job: Option<ChildJob>,
    stdin: Option<ChildStdin>,
    output: Arc<Mutex<String>>,
    readers: Vec<std::thread::JoinHandle<()>>,
    failure_seen: Option<Instant>,
    verify_account: Option<Account>,
    verification: Option<std::sync::mpsc::Receiver<Option<LoginStatus>>>,
    pub(crate) verified_status: Option<LoginStatus>,
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

fn spawn_reader(pipe: Box<dyn std::io::Read + Send>, out: Arc<Mutex<String>>) -> std::thread::JoinHandle<()> {
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
    })
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
        cmd.spawn().map(|child| {
            let mut session = Self::from_child(&acc.id, child);
            session.verify_account = Some(acc.clone());
            session
        }).map_err(|e| format!("{}: {e}", prog.display()))
    }

    fn from_child(account_id: &str, mut child: Child) -> Self {
        #[cfg(windows)]
        let job = ChildJob::attach(&child).ok();
        let output = Arc::new(Mutex::new(String::new()));
        let mut readers = Vec::with_capacity(2);
        if let Some(pipe) = child.stdout.take() {
            readers.push(spawn_reader(Box::new(pipe), output.clone()));
        }
        if let Some(pipe) = child.stderr.take() {
            readers.push(spawn_reader(Box::new(pipe), output.clone()));
        }
        Self {
            account_id: account_id.into(),
            stdin: child.stdin.take(),
            child,
            #[cfg(windows)]
            job,
            output,
            readers,
            failure_seen: None,
            verify_account: None,
            verification: None,
            verified_status: None,
            view: SessionView { account_id: account_id.into(), ..Default::default() },
            started: time::now(),
        }
    }

    /// 출력과 프로세스 상태를 읽어 진행 상황 갱신
    pub fn poll(&mut self) -> &SessionView {
        if self.view.done.is_some() {
            return &self.view;
        }
        match self.child.try_wait() {
            Ok(Some(exit_status)) if exit_status.success() => {
                self.view.done = Some(true);
                self.stdin = None;
                if let Some(account) = self.verify_account.take() {
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let result = status(&account);
                        let _ = tx.send((result.logged_in == Some(true)).then_some(result));
                    });
                    self.verification = Some(rx);
                }
                if let Some(verification) = &self.verification {
                    match verification.try_recv() {
                        Ok(Some(status)) => {
                            self.verified_status = Some(status);
                            self.view.message = None;
                        }
                        Err(std::sync::mpsc::TryRecvError::Empty) => {
                            self.view.done = None;
                            self.view.accepts_code = false;
                            self.view.url = None;
                            self.view.message = Some(crate::tr!("로그인 확인 중", "Verifying sign-in").into());
                        }
                        _ => {
                            self.view.done = Some(false);
                            self.view.message = Some(crate::tr!("사용 가능한 로그인 확인 실패", "Could not verify a usable sign-in").into());
                        }
                    }
                }
            }
            Ok(Some(status)) => {
                // Let the reader catch up after exit, but don't wait for pipes held by descendants.
                let seen = self.failure_seen.get_or_insert_with(Instant::now);
                if self.readers.iter().all(std::thread::JoinHandle::is_finished) || seen.elapsed() >= Duration::from_millis(400) {
                    let text = self.output.lock().map(|o| strip_ansi(&o)).unwrap_or_default();
                    let last = text.lines().rev().map(str::trim).find(|l| !l.is_empty());
                    self.view.done = Some(false);
                    self.view.message = Some(last.map_or_else(
                        || crate::tr!(format!("종료 코드 {:?}", status.code()), format!("Exit code {:?}", status.code())),
                        str::to_owned,
                    ));
                    self.stdin = None;
                }
            }
            Ok(None) if time::now() - self.started > 600.0 => {
                self.cancel();
                self.view.message =
                    Some(crate::tr!("10분 동안 로그인이 끝나지 않아 취소", "Sign-in canceled after 10 minutes without finishing").into());
            }
            Ok(None) => {
                let text = self.output.lock().map(|o| strip_ansi(&o)).unwrap_or_default();
                if let Some(url) = find_url(&text) {
                    self.view.url = Some(url);
                }
                let low = text.to_lowercase();
                self.view.accepts_code = ["paste code", "enter code", "authorization code", "paste the code"]
                    .iter().any(|prompt| low.contains(prompt));
            }
            Err(error) => {
                self.cancel();
                self.view.message = Some(crate::tr!(
                    format!("CLI 로그인 상태 확인 실패 ({error})"),
                    format!("Could not check the CLI sign-in ({error})")
                ));
            }
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
            #[cfg(windows)]
            drop(self.job.take());
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
    fn login_completion_uses_the_exit_code_and_preserves_cli_errors() {
        const MODE: &str = "USAGI_TEST_LOGIN_COMPLETION";
        if let Ok(mode) = std::env::var(MODE) {
            if mode == "failure" {
                writeln!(std::io::stderr(), "provider sign-in failure").unwrap();
                std::process::exit(9);
            }
            eprintln!("Transient error before opening the browser");
            return;
        }
        for (mode, success) in [("success", true), ("failure", false)] {
            let child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "login::session_tests::login_completion_uses_the_exit_code_and_preserves_cli_errors", "--nocapture"])
                .env(MODE, mode)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn().unwrap();
            let mut session = LoginSession::from_child("fixture", child);
            let end = Instant::now() + Duration::from_secs(4);
            while session.poll().done.is_none() && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(session.view().done, Some(success));
            if !success {
                assert_eq!(session.view().message.as_deref(), Some("provider sign-in failure"));
            }
        }
    }

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
