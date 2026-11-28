//! Codex(ChatGPT) 사용량 — Codex가 남기는 로컬 로그(~/.codex/sessions/**/rollout-*.jsonl)에서.
//!
//! Codex는 로그에 서버가 알려준 사용률을 그대로 적어 둡니다:
//!   "rate_limits":{"primary":{"used_percent":44.0,"window_minutes":10080,"resets_at":1795247489},...}
//! 파일에 저장된 ChatGPT 로그인은 HTTPS로 사용량을 읽고, 필요할 때 Codex app-server에 맡깁니다.

mod http;

use crate::logs::home_dir;
use crate::time::{self, DAY};
use crate::usage_retry::{self, UsageError, UsageMode};
use memchr::memmem;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CodexWindow {
    pub minutes: u32,
    pub used_percent: f64,
    pub resets_at: Option<f64>,
}

impl CodexWindow {
    /// 지금 기준 사용률. 리셋 시각이 지났으면 0.
    pub fn ratio_at(&self, now: f64) -> f64 {
        match self.resets_at {
            Some(r) if r <= now => 0.0,
            _ => (self.used_percent / 100.0).max(0.0),
        }
    }

    /// The provider's reset, while it is still in the future.
    pub fn next_reset(&self, now: f64) -> Option<f64> {
        self.resets_at.filter(|&reset| reset > now)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CodexSnapshot {
    pub ts: f64,
    pub windows: Vec<CodexWindow>,
    pub plan: Option<String>,
    pub reached: bool,
    /// 계정 이메일
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// 크레딧 잔액 (app-server)
    #[serde(default)]
    pub credits: Option<Credits>,
    /// 리필권 = 사용 한도 초기화권 (app-server의 rateLimitResetCredits)
    #[serde(default)]
    pub refills: Option<Refills>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Credits {
    pub has: bool,
    pub unlimited: bool,
    pub balance: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Refills {
    /// 쓸 수 있는 리필권 수
    pub available: u32,
    /// 가장 먼저 만료되는 리필권의 만료 시각
    pub expires_at: Option<f64>,
}

#[derive(Deserialize)]
struct RawWindow {
    used_percent: Option<f64>,
    window_minutes: Option<u32>,
    resets_at: Option<f64>,
}

#[derive(Deserialize)]
struct RawLimits {
    primary: Option<RawWindow>,
    secondary: Option<RawWindow>,
    plan_type: Option<String>,
    rate_limit_reached_type: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawPayload {
    rate_limits: Option<RawLimits>,
}

#[derive(Deserialize)]
struct RawLine {
    timestamp: Option<String>,
    payload: Option<RawPayload>,
}

pub fn default_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(env) = std::env::var_os("CODEX_HOME") {
        let p = PathBuf::from(env);
        roots.push(p.join("sessions"));
        roots.push(p.join("archived_sessions"));
    }
    if let Some(home) = home_dir() {
        roots.push(home.join(".codex").join("sessions"));
        roots.push(home.join(".codex").join("archived_sessions"));
    }
    roots
}

pub fn parse_line(line: &[u8]) -> Option<CodexSnapshot> {
    let raw: RawLine = serde_json::from_slice(line).ok()?;
    let limits = raw.payload?.rate_limits?;
    let mut windows = Vec::with_capacity(2);
    for w in [limits.primary, limits.secondary].into_iter().flatten() {
        if let (Some(p), Some(m)) = (w.used_percent.filter(|p| p.is_finite()), w.window_minutes) {
            windows.push(CodexWindow { minutes: m, used_percent: p, resets_at: w.resets_at.and_then(time::timestamp) });
        }
    }
    if windows.is_empty() {
        return None;
    }
    windows.sort_by_key(|w| w.minutes); // 짧은 창(5시간)이 위
    Some(CodexSnapshot {
        ts: raw.timestamp.as_deref().and_then(time::parse_iso).unwrap_or(0.0),
        windows,
        plan: limits.plan_type,
        reached: limits.rate_limit_reached_type.is_some_and(|v| !v.is_null()),
        email: None,
        workspace_id: None,
        credits: None,
        refills: None,
    })
}

fn session_started_after(file: &mut File, connected_at: f64, buf: &mut Vec<u8>) -> bool {
    buf.clear();
    if BufReader::new(file.take(256 * 1024)).read_until(b'\n', buf).is_err() {
        return false;
    }
    let Ok(meta) = serde_json::from_slice::<serde_json::Value>(buf) else { return false };
    meta["type"].as_str() == Some("session_meta")
        && meta["timestamp"].as_str().and_then(time::parse_iso).is_some_and(|at| at >= connected_at)
        && meta["payload"]["timestamp"].as_str().is_none_or(|timestamp| {
            time::parse_iso(timestamp).is_some_and(|at| at >= connected_at)
        })
}

/// 파일 끝부분에서 현재 연결에 해당하는 마지막 rate_limits 줄을 찾습니다.
fn last_snapshot(f: &mut File, start: u64, len: u64, connected_at: Option<f64>, buf: &mut Vec<u8>) -> std::io::Result<(u64, Option<CodexSnapshot>)> {
    let offset = len.saturating_sub(4 << 20).max(start);
    let read_at = offset.saturating_sub(1);
    f.seek(SeekFrom::Start(read_at))?;
    buf.clear();
    f.by_ref().take(len - read_at).read_to_end(buf)?;
    let next = read_at + memchr::memrchr(b'\n', buf).map_or(0, |i| i as u64 + 1);
    let finder = memmem::FinderRev::new(b"\"rate_limits\"");
    let mut end = buf.len();
    while let Some(pos) = finder.rfind(&buf[..end]) {
        let line_start = memchr::memrchr(b'\n', &buf[..pos]).map(|i| i + 1).unwrap_or(0);
        let line_end = memchr::memchr(b'\n', &buf[pos..]).map(|i| pos + i).unwrap_or(buf.len());
        if line_start > 0 || offset == 0 {
            if let Some(s) = parse_line(&buf[line_start..line_end]).filter(|s| connected_at.is_none_or(|at| s.ts >= at)) {
                return Ok((next.max(start), Some(s)));
            }
        }
        end = line_start.saturating_sub(1);
        if end == 0 {
            break;
        }
    }
    Ok((next.max(start), None))
}

struct Rollout {
    offset: u64,
    len: u64,
    modified: f64,
    accepted: bool,
    // Keep cursor entries small even for sessions that never reported a quota.
    snapshot: Option<Box<CodexSnapshot>>,
}

pub struct CodexReader {
    pub roots: Vec<PathBuf>,
    pub max_age_days: f64,
    files: std::collections::HashMap<PathBuf, Rollout>,
    pub snapshot: Option<CodexSnapshot>,
    connected_at: Option<f64>,
}

impl CodexReader {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self { roots, max_age_days: 30.0, files: Default::default(), snapshot: None, connected_at: None }
    }

    pub(crate) fn for_connection(roots: Vec<PathBuf>, revision: u64) -> Self {
        Self { connected_at: (revision != 0).then_some(revision as f64 / 1_000_000.0), ..Self::new(roots) }
    }

    pub fn rescan(&mut self) -> bool {
        let mut listed = Vec::new();
        for root in &self.roots {
            crate::logs::walk_files_with_meta(root, "jsonl", &mut listed);
        }
        let present: std::collections::HashSet<&PathBuf> = listed.iter().map(|(path, _)| path).collect();
        self.files.retain(|path, _| present.contains(path));
        // 목록 정보로 확실한 파일은 열지 않음 (logs::rescan_needs_check)
        let now = time::now();
        let cutoff = (now - self.max_age_days * DAY).max(self.connected_at.unwrap_or(0.0));
        let check: Vec<&Path> = listed
            .iter()
            .filter(|(path, meta)| crate::logs::rescan_needs_check(*meta, self.files.get(path).map(|f| (f.len, f.modified)), cutoff, now))
            .map(|(path, _)| path.as_path())
            .collect();
        self.update_paths(check.into_iter().map(|path| (path, false)))
    }

    /// Pick the newest usage event across concurrent rollouts.
    pub(crate) fn update_paths<'a>(&mut self, paths: impl IntoIterator<Item = (&'a Path, bool)>) -> bool {
        let cutoff = (time::now() - self.max_age_days * DAY).max(self.connected_at.unwrap_or(0.0));
        let mut buffer = Vec::new();
        for (path, replaced) in paths {
            if replaced {
                self.files.remove(path);
            }
            let meta = match std::fs::symlink_metadata(path) {
                Ok(meta) => meta,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.files.remove(path);
                    continue;
                }
                Err(_) => continue,
            };
            let mtime = crate::logs::mtime_secs(&meta);
            if !meta.is_file() || mtime < cutoff {
                self.files.remove(path);
                continue;
            }
            let len = meta.len();
            let prior = self.files.get_mut(path);
            if prior.as_ref().is_some_and(|file| file.len == len && file.modified == mtime) {
                continue;
            }
            let Ok(mut file) = File::open(path) else { continue };
            let append = prior.as_ref().filter(|old| old.accepted && len > old.len);
            let keep_snapshot = append.is_some();
            let accepted = keep_snapshot || self.connected_at.is_none_or(|at| session_started_after(&mut file, at, &mut buffer));
            let start = append.map_or(0, |old| old.offset);
            let (offset, snapshot) = if accepted {
                let Ok(result) = last_snapshot(&mut file, start, len, self.connected_at, &mut buffer) else { continue };
                result
            } else { (0, None) };
            let mut current = Rollout { offset, len, modified: mtime, accepted, snapshot: snapshot.map(Box::new) };
            if let Some(previous) = prior {
                if keep_snapshot && current.snapshot.is_none() {
                    current.snapshot = previous.snapshot.take();
                }
                *previous = current;
            } else {
                self.files.insert(path.to_owned(), current);
            }
        }
        let best = self.files.iter().filter_map(|(path, file)| Some((path, file.snapshot.as_deref()?)))
            .max_by(|(path_a, a), (path_b, b)| a.ts.total_cmp(&b.ts).then_with(|| path_a.cmp(path_b)))
            .map(|(_, snapshot)| snapshot);
        let changed = self.snapshot.as_ref() != best;
        if changed { self.snapshot = best.cloned(); }
        changed
    }
}

/// 창 길이 → 라벨: 300분 "5시간", 10080분 "주간"
pub fn window_label(minutes: u32) -> String {
    match minutes {
        10080 => "주간".to_string(),
        m if m % 1440 == 0 => format!("{}일", m / 1440),
        m if m % 60 == 0 => format!("{}시간", m / 60),
        m => format!("{m}분"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_line(at: &str) -> String {
        serde_json::json!({"type": "session_meta", "timestamp": at, "payload": {"timestamp": at}}).to_string() + "\n"
    }

    fn quota_line(at: &str, percent: u32) -> String {
        serde_json::json!({
            "timestamp": at, "type": "event_msg",
            "payload": { "rate_limits": { "primary": { "used_percent": percent, "window_minutes": 10080 } } }
        }).to_string() + "\n"
    }

    #[test]
    fn new_connection_skips_old_sessions_even_if_they_are_still_writing() {
        let dir = tempfile::tempdir().unwrap();
        let before = "2025-01-01T15:00:00Z";
        let after = "2025-01-01T16:00:00Z";
        let revision = (time::parse_iso("2025-01-01T15:30:00Z").unwrap() * 1_000_000.0) as u64;
        std::fs::write(dir.path().join("old.jsonl"), session_line(before) + &quota_line(after, 80)).unwrap();
        std::fs::write(dir.path().join("unidentified.jsonl"), quota_line(after, 90)).unwrap();
        let mut reader = CodexReader::for_connection(vec![dir.path().to_owned()], revision);
        assert!(!reader.rescan());
        assert!(reader.snapshot.is_none());
        std::fs::write(dir.path().join("new.jsonl"), session_line(after) + &quota_line(after, 10)).unwrap();
        assert!(reader.rescan());
        assert_eq!(reader.snapshot.as_ref().unwrap().windows[0].used_percent, 10.0);
    }

    #[test]
    fn appending_to_the_current_session_does_not_rescan_the_folders() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tracked.jsonl");
        let at = "2025-01-01T16:00:00Z";
        std::fs::write(&path, quota_line(at, 20)).unwrap();
        let mut reader = CodexReader::new(vec![dir.path().to_owned()]);
        assert!(reader.rescan());
        let mut log = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        log.write_all(quota_line(at, 50).as_bytes()).unwrap();
        std::fs::write(dir.path().join("missed.jsonl"), quota_line("2025-01-01T16:01:00Z", 60)).unwrap();
        assert!(reader.update_paths([(path.as_path(), false)]));
        assert_eq!(reader.snapshot.as_ref().unwrap().windows[0].used_percent, 50.0);
        assert!(reader.rescan());
        assert_eq!(reader.snapshot.as_ref().unwrap().windows[0].used_percent, 60.0);
    }

    #[test]
    fn chooses_newest_quota_event_instead_of_last_modified_rollout() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let older_event = dir.path().join("older.jsonl");
        let newer_event = dir.path().join("newer.jsonl");
        std::fs::write(&newer_event, quota_line("2026-11-23T16:10:00Z", 40)).unwrap();
        std::fs::write(&older_event, quota_line("2026-11-23T16:00:00Z", 10)).unwrap();
        let mut reader = CodexReader::new(vec![dir.path().to_owned()]);
        assert!(reader.rescan());
        assert_eq!(reader.snapshot.as_ref().unwrap().windows[0].used_percent, 40.0);

        let mut older = std::fs::OpenOptions::new().append(true).open(&older_event).unwrap();
        older.write_all(quota_line("2026-11-23T16:20:00Z", 55).as_bytes()).unwrap();
        assert!(reader.update_paths([(older_event.as_path(), false)]));
        assert_eq!(reader.snapshot.as_ref().unwrap().windows[0].used_percent, 55.0);
    }

    #[test]
    fn parses_rate_limits_line() {
        let line = br#"{"timestamp":"2026-11-15T10:12:00.000Z","type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":{"limit_id":"codex","limit_name":null,"primary":{"used_percent":44.0,"window_minutes":10080,"resets_at":1795247489},"secondary":{"used_percent":12.5,"window_minutes":300,"resets_at":1795200000},"plan_type":"plus","rate_limit_reached_type":null}}}"#;
        let s = parse_line(line).unwrap();
        assert_eq!(s.windows.len(), 2);
        assert_eq!(s.windows[0].minutes, 300);
        assert_eq!(window_label(s.windows[0].minutes), "5시간");
        assert_eq!(window_label(s.windows[1].minutes), "주간");
        assert!(!s.reached);
        assert_eq!(s.windows[1].ratio_at(1_795_247_490.0), 0.0); // 리셋 지남
        assert!((s.windows[1].ratio_at(1_795_000_000.0) - 0.44).abs() < 1e-9);
        assert_eq!(s.windows[1].next_reset(1_795_247_490.0), None);
    }

    #[test]
    fn parses_app_server_credits_and_refills() {
        let r: serde_json::Value = serde_json::from_str(
            r#"{"rateLimits":{"primary":{"usedPercent":7,"windowDurationMins":10080,"resetsAt":1795852517},"secondary":null,
                "credits":{"hasCredits":false,"unlimited":false,"balance":"12.5"},"planType":"pro","rateLimitReachedType":null},
                "rateLimitResetCredits":{"availableCount":1,"credits":[{"id":"x","resetType":"codexRateLimits","status":"available","grantedAt":1794522134,"expiresAt":1797114134,"title":"Full reset"}]}}"#,
        )
        .unwrap();
        let s = parse_app_server(&r, 1_795_000_000.0).unwrap();
        assert_eq!(s.plan.as_deref(), Some("pro"));
        assert_eq!(s.credits, Some(Credits { has: false, unlimited: false, balance: 12.5 }));
        assert_eq!(s.refills, Some(Refills { available: 1, expires_at: Some(1_797_114_134.0) }));
        assert_eq!(plan_label("pro"), "Pro 200");
        assert_eq!(plan_label("prolite"), "Pro 100");
        assert_eq!(plan_label("promax"), "Pro 500");
        assert_eq!(plan_label("plus"), "Plus");
        assert_eq!(plan_label("new_tier"), "New Tier");
    }
}

// ------------------------------------------------------------------ app-server (토큰 소모 없음)

/// 공식 Codex CLI의 `app-server`에 계정 사용률을 물어봅니다 (Codex VS Code 확장과 같은 방식).
/// 모델을 부르지 않으므로 토큰을 쓰지 않고, 사용량 창을 새로 열지도 않습니다.
/// 로그인 토큰은 Codex가 직접 다루며 usAgI는 보지 않습니다.
fn app_server_usage(acc: &crate::accounts::Account) -> Result<CodexSnapshot, String> {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    use std::time::Duration;

    let mut last_err = String::from(crate::tr!("Codex CLI 찾기 실패", "Codex CLI not found"));
    let identity_before = http::profile_identity(&acc.path());
    for prog in crate::login::programs_or_hint("codex")? {
        let mut cmd = crate::login::quiet_command(&prog);
        cmd.arg("app-server").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        if let Some((k, v)) = crate::login::profile_env(acc) {
            cmd.env(k, v);
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                last_err = format!("{}: {e}", prog.display());
                continue;
            }
        };
        // 끝낼 때 Codex가 띄운 하위 프로세스(.cmd·node 경유로 실행된 실제 app-server)까지 함께 종료
        #[cfg(windows)]
        let _job = crate::login::ChildJob::attach(&child).ok();
        let mut stdin = child.stdin.take().ok_or("stdin")?;
        let stdout = child.stdout.take().ok_or("stdout")?;
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let init = format!(
            r#"{{"method":"initialize","id":1,"params":{{"clientInfo":{{"name":"usagi","title":"usAgI","version":"{}"}}}}}}"#,
            crate::VERSION
        );
        let msgs = [
            init.as_str(),
            r#"{"method":"initialized"}"#,
            r#"{"method":"account/rateLimits/read","id":2}"#,
            r#"{"method":"account/read","id":3,"params":{}}"#,
        ];
        let mut email: Option<String> = None;
        let mut plan: Option<String> = None;
        let mut got = (false, false);
        let mut result = Err(crate::tr!("Codex 사용량 응답 없음", "No usage response from Codex").to_string());
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut sent = 0;
        // initialize 응답을 받은 뒤 나머지를 보냄
        if writeln!(stdin, "{}", msgs[0]).is_ok() {
            sent = 1;
        }
        while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
            let Ok(line) = rx.recv_timeout(left) else { break };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            match v.get("id").and_then(|i| i.as_i64()) {
                Some(1) if sent == 1 => {
                    let _ = writeln!(stdin, "{}", msgs[1]);
                    let _ = writeln!(stdin, "{}", msgs[2]);
                    let _ = writeln!(stdin, "{}", msgs[3]);
                    let _ = stdin.flush();
                    sent = 4;
                }
                Some(2) => {
                    result = match v.get("result") {
                        Some(r) => parse_app_server(r, crate::time::now()),
                        None => Err(v
                            .get("error")
                            .and_then(|e| e.get("message"))
                            .and_then(|m| m.as_str())
                            .unwrap_or(crate::tr!("오류", "Error"))
                            .to_string()),
                    };
                    got.0 = true;
                }
                Some(3) => {
                    let acc = v.get("result").and_then(|r| r.get("account"));
                    email = acc.and_then(|a| a.get("email")).and_then(|e| e.as_str()).map(str::to_string);
                    plan = acc.and_then(|a| a.get("planType")).and_then(|e| e.as_str()).map(str::to_string);
                    got.1 = true;
                }
                _ => {}
            }
            if got.0 && got.1 {
                break;
            }
        }
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        return result.map(|mut s| {
            let identity_after = http::profile_identity(&acc.path());
            if let Some((workspace, file_email)) = identity_after.filter(|identity| Some(identity) == identity_before.as_ref()) {
                if email.as_deref().is_some_and(|email| email.eq_ignore_ascii_case(&file_email)) {
                    s.workspace_id = Some(workspace);
                }
            }
            s.email = email;
            s.plan = plan.or(s.plan);
            s
        });
    }
    Err(last_err)
}

/// Read the current quotas without starting Codex when its file-based session is usable.
pub fn read_usage(acc: &crate::accounts::Account) -> Result<CodexSnapshot, String> {
    refresh_usage(acc, UsageMode::WithCli).map_err(|error| error.to_string())
}

pub(crate) fn refresh_usage(acc: &crate::accounts::Account, mode: UsageMode) -> Result<CodexSnapshot, UsageError> {
    usage_retry::run(&acc.usage_key(), || match http::read(acc) {
        Ok(Some(snapshot)) => Ok(snapshot),
        Ok(None) if mode == UsageMode::ApiOnly => Err(UsageError::NoApiSession),
        Ok(None) => http::recover_with_cli(&acc.path(), || app_server_usage(acc)),
        Err(error) => Err(error),
    })
}

pub(crate) fn recover_usage(acc: &crate::accounts::Account) -> Result<CodexSnapshot, UsageError> {
    usage_retry::run(&acc.usage_key(), || http::recover_with_cli(&acc.path(), || app_server_usage(acc)))
}

pub(crate) fn profile_identity(dir: &Path) -> Option<(String, String)> {
    http::profile_identity(dir)
}

pub(crate) fn file_login_status(dir: &Path) -> Option<crate::login::LoginStatus> {
    http::login_status(dir)
}

/// Codex(app-server)가 알려 주는 요금제 ID → 표시 이름.
/// ID 목록은 Codex app-server 스키마의 PlanType, 이름은 OpenAI 도움말의 요금제 이름 (Pro 100/200/500).
pub fn plan_label(id: &str) -> String {
    let id = id.trim().to_lowercase();
    let known = match id.as_str() {
        "free" => "Free",
        "go" => "Go",
        "plus" => "Plus",
        "prolite" => "Pro 100",
        "pro" => "Pro 200",
        "promax" => "Pro 500",
        "team" => "Team",
        "business" | "self_serve_business_prolite" | "self_serve_business_usage_based" => "Business",
        "enterprise" | "ent26" | "enterprise_cbp_automation" | "enterprise_cbp_usage_based" => "Enterprise",
        "edu" => "Edu",
        "edu_plus" => "Edu Plus",
        "edu_pro" => "Edu Pro",
        "unknown" | "" => "",
        _ => return id.split('_').filter(|w| !w.is_empty()).map(|w| { let mut c = w.chars(); c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default() }).collect::<Vec<_>>().join(" "),
    };
    known.to_string()
}

pub fn parse_app_server(r: &serde_json::Value, now: f64) -> Result<CodexSnapshot, String> {
    let limits = r.get("rateLimits").ok_or(crate::tr!("사용량 정보 없음", "No usage info"))?;
    let mut windows = Vec::new();
    for key in ["primary", "secondary"] {
        if let Some(w) = limits.get(key).filter(|w| !w.is_null()) {
            if let (Some(p), Some(m)) = (w.get("usedPercent").and_then(|x| x.as_f64()).filter(|p| p.is_finite()), w.get("windowDurationMins").and_then(|x| x.as_u64())) {
                windows.push(CodexWindow { minutes: m as u32, used_percent: p, resets_at: w.get("resetsAt").and_then(|x| x.as_f64()).and_then(time::timestamp) });
            }
        }
    }
    if windows.is_empty() {
        return Err(crate::tr!("사용량 창 정보 없음", "No usage window info").into());
    }
    windows.sort_by_key(|w| w.minutes);
    Ok(CodexSnapshot {
        ts: now,
        windows,
        plan: limits.get("planType").and_then(|p| p.as_str()).map(str::to_string),
        reached: limits.get("rateLimitReachedType").is_some_and(|v| !v.is_null()),
        email: None,
        workspace_id: None,
        credits: limits.get("credits").filter(|c| !c.is_null()).map(|c| Credits {
            has: c.get("hasCredits").and_then(|v| v.as_bool()).unwrap_or(false),
            unlimited: c.get("unlimited").and_then(|v| v.as_bool()).unwrap_or(false),
            balance: c
                .get("balance")
                .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
                .filter(|balance: &f64| balance.is_finite())
                .unwrap_or(0.0),
        }),
        refills: r.get("rateLimitResetCredits").filter(|c| !c.is_null()).map(|c| {
            let list = c.get("credits").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let usable = list.iter().filter(|x| x.get("status").and_then(|s| s.as_str()).is_none_or(|s| s == "available"));
            Refills {
                available: c.get("availableCount").and_then(|v| v.as_u64()).map(|v| v as u32).unwrap_or_else(|| usable.clone().count() as u32),
                expires_at: usable.filter_map(|x| x.get("expiresAt").and_then(|v| v.as_f64()).and_then(time::timestamp)).min_by(|a, b| a.total_cmp(b)),
            }
        }),
    })
}

pub(crate) fn saved_path(account_id: &str) -> PathBuf {
    let safe: String = account_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    crate::config::config_dir().join(format!("codex-usage-{safe}.json"))
}

pub fn save(account_id: &str, s: &CodexSnapshot) -> std::io::Result<()> {
    crate::config::write_json(&saved_path(account_id), s)
}

pub fn load(account_id: &str) -> Option<CodexSnapshot> {
    serde_json::from_str(&std::fs::read_to_string(saved_path(account_id)).ok()?).ok()
}

#[cfg(test)]
mod app_server_tests {
    use super::*;

    #[test]
    fn parses_rate_limits_read() {
        let r = serde_json::json!({"ordinaryUsageAllowed":true,"rateLimits":{"limitId":"codex","primary":{"usedPercent":7,"windowDurationMins":10080,"resetsAt":1795852517},"secondary":null,"planType":"pro","rateLimitReachedType":null}});
        let s = parse_app_server(&r, 5.0).unwrap();
        assert_eq!(s.windows.len(), 1);
        assert_eq!(s.windows[0].minutes, 10080);
        assert_eq!(s.windows[0].used_percent, 7.0);
        assert_eq!(s.plan.as_deref(), Some("pro"));
        assert!(!s.reached);
        assert_eq!(s.ts, 5.0);
    }

    #[test]
    fn absurd_numbers_from_codex_are_rejected_or_clamped() {
        let r = serde_json::json!({"rateLimits":{"primary":{"usedPercent":7,"windowDurationMins":300,"resetsAt":1e300},
            "credits":{"hasCredits":true,"unlimited":false,"balance":"inf"}},
            "rateLimitResetCredits":{"availableCount":1,"credits":[{"status":"available","expiresAt":1e300}]}});
        let s = parse_app_server(&r, 5.0).unwrap();
        assert_eq!(s.windows[0].resets_at, Some(time::MAX_TIMESTAMP));
        assert_eq!(s.credits.unwrap().balance, 0.0);
        assert_eq!(s.refills.unwrap().expires_at, Some(time::MAX_TIMESTAMP));
        let line = br#"{"timestamp":"2026-11-15T10:12:00.000Z","payload":{"rate_limits":{"primary":{"used_percent":1.0,"window_minutes":300,"resets_at":1e300}}}}"#;
        assert_eq!(parse_line(line).unwrap().windows[0].resets_at, Some(time::MAX_TIMESTAMP));
    }
}
