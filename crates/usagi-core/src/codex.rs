//! Codex(ChatGPT) 사용량 — Codex가 남기는 로컬 로그(~/.codex/sessions/**/rollout-*.jsonl)에서.
//!
//! Codex는 로그에 서버가 알려준 사용률을 그대로 적어 둡니다:
//!   "rate_limits":{"primary":{"used_percent":44.0,"window_minutes":10080,"resets_at":1795247489},...}
//! 그래서 추정 없이 정확한 값을 씁니다. 거기에 더해 Codex `app-server`로 지금 값을 직접 읽습니다(토큰 소모 없음).

use crate::logs::{home_dir, walk_files};
use crate::time::{self, DAY};
use memchr::memmem;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
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

    /// 다음 리셋 시각 (지난 리셋이면 창 길이만큼 넘겨서 추정)
    pub fn next_reset(&self, now: f64) -> Option<f64> {
        let mut r = self.resets_at?;
        let span = self.minutes as f64 * 60.0;
        if span > 0.0 {
            while r <= now {
                r += span;
            }
        }
        Some(r)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CodexSnapshot {
    pub ts: f64,
    pub windows: Vec<CodexWindow>,
    pub plan: Option<String>,
    pub reached: bool,
    /// 계정 이메일 (app-server의 account/read)
    #[serde(default)]
    pub email: Option<String>,
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
    let mut windows = Vec::new();
    for w in [limits.primary, limits.secondary].into_iter().flatten() {
        if let (Some(p), Some(m)) = (w.used_percent, w.window_minutes) {
            windows.push(CodexWindow { minutes: m, used_percent: p, resets_at: w.resets_at });
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
        credits: None,
        refills: None,
    })
}

/// 파일 끝부분에서 마지막 rate_limits 줄을 찾습니다 (큰 파일도 꼬리만 읽음).
fn last_snapshot(path: &Path) -> Option<CodexSnapshot> {
    let mut f = File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut tail = 1u64 << 20;
    loop {
        let start = len.saturating_sub(tail);
        f.seek(SeekFrom::Start(start)).ok()?;
        let mut buf = Vec::with_capacity((len - start) as usize);
        f.by_ref().take(len - start).read_to_end(&mut buf).ok()?;
        let finder = memmem::FinderRev::new(b"\"rate_limits\"");
        let mut end = buf.len();
        while let Some(pos) = finder.rfind(&buf[..end]) {
            let line_start = memchr::memrchr(b'\n', &buf[..pos]).map(|i| i + 1).unwrap_or(0);
            let line_end = memchr::memchr(b'\n', &buf[pos..]).map(|i| pos + i).unwrap_or(buf.len());
            if line_start > 0 || start == 0 {
                if let Some(s) = parse_line(&buf[line_start..line_end]) {
                    return Some(s);
                }
            }
            end = line_start.saturating_sub(1);
            if end == 0 {
                break;
            }
        }
        if start == 0 || tail >= 64 << 20 {
            return None;
        }
        tail *= 4;
    }
}

pub struct CodexReader {
    pub roots: Vec<PathBuf>,
    pub max_age_days: f64,
    last_file: Option<(PathBuf, u64, f64)>,
    pub snapshot: Option<CodexSnapshot>,
    /// 마지막으로 폴더 전체를 훑은 시각
    last_full: f64,
}

impl CodexReader {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Self { roots, max_age_days: 30.0, last_file: None, snapshot: None, last_full: 0.0 }
    }

    /// 가장 최근에 바뀐 rollout 파일을 다시 확인. 바뀌었으면 true.
    pub fn poll(&mut self) -> bool {
        let now = time::now();
        // 폴더 전체는 가끔만 훑고, 그 사이엔 지금 보고 있는 파일이 바뀌었는지만 확인
        if now - self.last_full < crate::logs::FULL_SCAN_SECS {
            match &self.last_file {
                Some((path, len, mtime)) => {
                    let same = std::fs::metadata(path).is_ok_and(|m| m.len() == *len && crate::logs::mtime_secs(&m) == *mtime);
                    if same {
                        return false;
                    }
                }
                None => return false,
            }
        }
        self.last_full = now;
        let mut files = Vec::new();
        for root in &self.roots {
            walk_files(root, "jsonl", &mut files);
        }
        let cutoff = now - self.max_age_days * DAY;
        let mut newest: Option<(PathBuf, u64, f64)> = None;
        for path in files {
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            if mtime >= cutoff && newest.as_ref().is_none_or(|n| mtime > n.2) {
                newest = Some((path, meta.len(), mtime));
            }
        }
        if newest == self.last_file {
            return false;
        }
        self.last_file = newest.clone();
        let snap = newest.and_then(|(p, _, _)| last_snapshot(&p));
        let changed = snap != self.snapshot;
        if snap.is_some() || self.last_file.is_none() {
            self.snapshot = snap;
        }
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
        assert_eq!(s.windows[1].next_reset(1_795_247_490.0), Some(1_795_247_489.0 + 10080.0 * 60.0));
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
pub fn read_via_app_server(acc: &crate::accounts::Account) -> Result<CodexSnapshot, String> {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    use std::time::Duration;

    let mut last_err = String::from(crate::tr!("Codex CLI 찾기 실패", "Codex CLI not found"));
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
            s.email = email;
            s.plan = plan.or(s.plan);
            s
        });
    }
    Err(last_err)
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
            if let (Some(p), Some(m)) = (w.get("usedPercent").and_then(|x| x.as_f64()), w.get("windowDurationMins").and_then(|x| x.as_u64())) {
                windows.push(CodexWindow { minutes: m as u32, used_percent: p, resets_at: w.get("resetsAt").and_then(|x| x.as_f64()) });
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
        credits: limits.get("credits").filter(|c| !c.is_null()).map(|c| Credits {
            has: c.get("hasCredits").and_then(|v| v.as_bool()).unwrap_or(false),
            unlimited: c.get("unlimited").and_then(|v| v.as_bool()).unwrap_or(false),
            balance: c
                .get("balance")
                .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
                .unwrap_or(0.0),
        }),
        refills: r.get("rateLimitResetCredits").filter(|c| !c.is_null()).map(|c| {
            let list = c.get("credits").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            let usable = list.iter().filter(|x| x.get("status").and_then(|s| s.as_str()).is_none_or(|s| s == "available"));
            Refills {
                available: c.get("availableCount").and_then(|v| v.as_u64()).map(|v| v as u32).unwrap_or_else(|| usable.clone().count() as u32),
                expires_at: usable.filter_map(|x| x.get("expiresAt").and_then(|v| v.as_f64())).min_by(|a, b| a.total_cmp(b)),
            }
        }),
    })
}

pub(crate) fn saved_path(account_id: &str) -> PathBuf {
    let safe: String = account_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    crate::config::config_dir().join(format!("codex-usage-{safe}.json"))
}

pub fn save(account_id: &str, s: &CodexSnapshot) {
    let _ = std::fs::create_dir_all(crate::config::config_dir());
    if let Ok(text) = serde_json::to_string(s) {
        let tmp = saved_path(account_id).with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, saved_path(account_id));
        }
    }
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
}
