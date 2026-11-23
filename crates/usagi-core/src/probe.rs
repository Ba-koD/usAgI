//! 사용량 확인 요청 — 공식 Claude Code CLI로 아주 작은 요청을 보내 정확한 사용률을 받습니다.
//!
//! `claude -p … --output-format stream-json --verbose` 출력에는 서버가 준 사용률이 실려 옵니다:
//!   {"type":"rate_limit_event","rate_limit_info":{"status":"allowed",
//!    "unifiedWindows":{"five_hour":{"utilization":0.14,"resetsAt":…},"seven_day":{"utilization":0.58,"resetsAt":…}}}}
//!
//! - 사용자의 Claude Code 로그인으로 Claude Code가 직접 요청합니다. usAgI는 토큰을 다루지 않습니다.
//! - 가장 작은 모델·한 줄 시스템 프롬프트·도구 없음·기록 안 남김 → 요청 1번 ≈ 입력 700 + 출력 100 토큰.
//! - Claude의 5시간 창은 첫 요청부터 시작하므로, 창이 닫혀 있을 때 자동으로 보내지 않습니다(엔진이 판단).

use crate::accounts::Account;
use crate::bridge::{BridgeFile, BridgeWindow};
use crate::config::config_dir;
use crate::login::{capture_child_output, profile_env, programs_or_hint};
use crate::time;
use std::process::Stdio;
use std::time::Duration;

pub const PROBE_ARGS: &[&str] = &[
    "-p",
    "ping",
    "--model",
    "haiku",
    "--system-prompt",
    "Reply with exactly: ok",
    "--tools",
    "",
    "--strict-mcp-config",
    "--disable-slash-commands",
    "--setting-sources",
    "local",
    "--no-session-persistence",
    "--output-format",
    "stream-json",
    "--verbose",
];

#[derive(Clone, Debug, PartialEq)]
pub struct ProbeOutcome {
    pub file: BridgeFile,
    /// "allowed" | "allowed_warning" | "rejected"
    pub status: String,
}

fn window(v: Option<&serde_json::Value>) -> Option<BridgeWindow> {
    let v = v?;
    let util = v.get("utilization")?.as_f64()?;
    Some(BridgeWindow {
        used_percentage: util * 100.0,
        resets_at: v.get("resetsAt").or_else(|| v.get("resets_at")).and_then(|r| r.as_f64()),
    })
}

/// stream-json 출력에서 사용률 찾기
pub fn parse_output(out: &str, now: f64) -> Result<ProbeOutcome, String> {
    let mut found: Option<ProbeOutcome> = None;
    let mut error: Option<String> = None;
    for line in out.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("rate_limit_event") => {
                let info = v.get("rate_limit_info");
                let windows = info.and_then(|i| i.get("unifiedWindows"));
                let mut five = window(windows.and_then(|w| w.get("five_hour")));
                let seven = window(windows.and_then(|w| w.get("seven_day")));
                let status = info.and_then(|i| i.get("status")).and_then(|s| s.as_str()).unwrap_or("").to_string();
                // 거절됐는데 창 정보가 없으면 5시간 한도 도달로 봄
                if status == "rejected" && five.is_none() {
                    let reset = info.and_then(|i| i.get("resetsAt")).and_then(|r| r.as_f64());
                    five = Some(BridgeWindow { used_percentage: 100.0, resets_at: reset });
                }
                if five.is_some() || seven.is_some() {
                    found = Some(ProbeOutcome {
                        file: BridgeFile { updated_at: now, five_hour: five, weekly_anchor: seven.and_then(|w| w.resets_at), seven_day: seven, ..Default::default() },
                        status,
                    });
                }
            }
            Some("result") if v.get("is_error").and_then(|e| e.as_bool()) == Some(true) => {
                error = v.get("result").and_then(|r| r.as_str()).map(str::to_string);
            }
            _ => {}
        }
    }
    match (found, error) {
        (Some(f), _) => Ok(f),
        (None, Some(e)) => Err(e),
        (None, None) => Err(crate::tr!("응답에 사용량 정보 없음", "No usage info in the response").into()),
    }
}

/// 확인 요청 실행 (2~5초). 백그라운드 스레드에서 부르세요.
pub fn run(acc: &Account) -> Result<ProbeOutcome, String> {
    let out = run_cli(acc, PROBE_ARGS, 90)?;
    parse_output(&out, time::now())
}

/// 공식 Claude Code CLI를 창 없이 실행하고 표준 출력을 돌려줌 (프로필 폴더 기준, 시간 제한)
pub(crate) fn run_cli(acc: &Account, args: &[&str], timeout_secs: u64) -> Result<String, String> {
    let mut last_err = String::from(crate::tr!("Claude Code CLI 찾기 실패", "Claude Code CLI not found"));
    for prog in programs_or_hint("claude")? {
        let mut cmd = crate::login::quiet_command(&prog);
        cmd.args(args)
            .current_dir(config_dir()) // 프로젝트 설정·CLAUDE.md를 읽지 않게
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some((k, v)) = profile_env(acc) {
            cmd.env(k, v);
        }
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                last_err = format!("{}: {e}", prog.display());
                continue;
            }
        };
        return capture_child_output(child, Duration::from_secs(timeout_secs))
            .map(|(stdout, _)| stdout)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::TimedOut {
                    crate::tr!(format!("확인 시간 초과 ({timeout_secs}초)"), format!("Check timed out ({timeout_secs}s)"))
                } else {
                    error.to_string()
                }
            });
    }
    Err(last_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rate_limit_event() {
        let out = r#"{"type":"system","subtype":"init"}
{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1795302000,"rateLimitType":"five_hour","unifiedWindows":{"five_hour":{"utilization":0.14,"resetsAt":1795302000},"seven_day":{"utilization":0.58,"resetsAt":1795309200}}}}
{"type":"result","subtype":"success","is_error":false,"result":"ok"}"#;
        let o = parse_output(out, 10.0).unwrap();
        assert_eq!(o.status, "allowed");
        let f = o.file;
        assert!((f.five_hour.unwrap().used_percentage - 14.0).abs() < 1e-9);
        assert_eq!(f.seven_day.unwrap().resets_at, Some(1_795_309_200.0));
        assert_eq!(f.weekly_anchor, Some(1_795_309_200.0));
        assert_eq!(f.updated_at, 10.0);
    }

    #[test]
    fn reports_cli_errors() {
        let out = r#"{"type":"result","subtype":"error","is_error":true,"result":"Invalid API key · Please run /login"}"#;
        assert_eq!(parse_output(out, 0.0).unwrap_err(), "Invalid API key · Please run /login");
        let rejected = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","resetsAt":500}}"#;
        let o = parse_output(rejected, 0.0).unwrap();
        assert_eq!(o.file.five_hour.unwrap().used_percentage, 100.0);
    }
}
