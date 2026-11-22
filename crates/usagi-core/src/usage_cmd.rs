//! Claude Code의 `/usage` 명령으로 정확한 사용량 받기.
//!
//! `claude -p "/usage"`는 Claude Code 안에서 처리되는 로컬 명령이라 모델 요청이 없습니다
//! (토큰 0, 5시간 창도 새로 열지 않음). 화면에 보이는 것과 같은 값이 나옵니다:
//!
//! ```text
//! Current session: 30% used · resets Nov 22, 8am (Asia/Seoul)
//! Current week (all models): 61% used · resets Nov 22, 10am (Asia/Seoul)
//! Current week (Fable): 0% used · resets Nov 22, 10am (Asia/Seoul)
//! ```
//!
//! 혹시 CLI가 이 명령을 모르는 버전이면 모델 요청으로 처리될 수 있으므로, 결과에 모델 사용이
//! 보이면 그 계정은 이번 실행 동안 다시 쓰지 않고 기존 확인 방식([`crate::probe`])으로 돌아갑니다.

use crate::accounts::Account;
use crate::bridge::{BridgeFile, BridgeWindow, ModelWindow};
use crate::logs::parse_reset;
use crate::probe::run_cli;
use std::collections::HashSet;
use std::sync::Mutex;

/// 혹시 모델로 넘어가더라도 가장 싼 모델·도구 없음으로
pub const USAGE_ARGS: &[&str] = &[
    "-p",
    "/usage",
    "--model",
    "haiku",
    "--tools",
    "",
    "--strict-mcp-config",
    "--setting-sources",
    "local",
    "--no-session-persistence",
    "--output-format",
    "json",
];

/// `/usage`를 지원하지 않는 것으로 확인된 계정 (이번 실행 동안)
static UNSUPPORTED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

pub fn supported(account_id: &str) -> bool {
    UNSUPPORTED.lock().ok().is_none_or(|g| g.as_ref().is_none_or(|s| !s.contains(account_id)))
}

fn mark_unsupported(account_id: &str) {
    if let Ok(mut g) = UNSUPPORTED.lock() {
        g.get_or_insert_with(HashSet::new).insert(account_id.to_string());
    }
}

/// `/usage` 글자 → 사용량 파일. 알아볼 줄이 하나도 없으면 None.
pub fn parse(text: &str, now: f64) -> Option<BridgeFile> {
    let mut file = BridgeFile { updated_at: now, ..Default::default() };
    let mut any = false;
    for raw in text.lines() {
        let line = raw.trim();
        let Some(rest) = line.strip_prefix("Current ") else { continue };
        let Some(colon) = rest.find(':') else { continue };
        let (head, tail) = (rest[..colon].trim(), &rest[colon + 1..]);
        let Some(pct) = tail.trim_start().split('%').next().and_then(|p| p.trim().parse::<f64>().ok()) else { continue };
        let resets_at = tail.to_lowercase().find("reset").and_then(|i| parse_reset(&tail[i..], now));
        let w = BridgeWindow { used_percentage: pct, resets_at };
        if head.eq_ignore_ascii_case("session") {
            file.five_hour = Some(w);
        } else if let Some(name) = head.strip_prefix("week (").and_then(|s| s.strip_suffix(')')) {
            if name.eq_ignore_ascii_case("all models") {
                file.seven_day = Some(w);
            } else {
                file.models.push(ModelWindow { name: name.trim().to_string(), window: w });
            }
        } else if head.eq_ignore_ascii_case("week") {
            file.seven_day = Some(w);
        } else {
            continue;
        }
        any = true;
    }
    file.weekly_anchor = file.seven_day.and_then(|w| w.resets_at);
    any.then_some(file)
}

/// 공식 CLI로 `/usage`를 실행해 사용량을 받습니다.
pub fn run(acc: &Account) -> Result<BridgeFile, String> {
    let out = run_cli(acc, USAGE_ARGS, 60)?;
    let v: serde_json::Value = serde_json::from_str(out.trim()).map_err(|_| "사용량 결과 읽기 실패".to_string())?;
    // 모델이 불렸다면 로컬 명령이 아님 → 다시 쓰지 않음 (토큰 낭비 방지)
    let model_used = v.get("modelUsage").and_then(|m| m.as_object()).is_some_and(|m| !m.is_empty())
        || v.get("total_cost_usd").and_then(|c| c.as_f64()).is_some_and(|c| c > 0.0);
    if model_used {
        mark_unsupported(&acc.id);
        return Err("이 Claude Code 버전은 /usage 바로 실행 불가".into());
    }
    let text = v.get("result").and_then(|r| r.as_str()).unwrap_or("");
    if v.get("is_error").and_then(|e| e.as_bool()) == Some(true) {
        return Err(text.lines().next().unwrap_or("사용량 수신 실패").to_string());
    }
    parse(text, crate::time::now()).ok_or_else(|| text.lines().next().filter(|l| !l.is_empty()).unwrap_or("사용량 정보 없음").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_usage_text_with_model_windows() {
        let sent = crate::time::to_ts_in(2026, 11, 22, 5, 10, &crate::time::tz_named("Asia/Seoul").unwrap()).unwrap();
        let text = "You are currently using your subscription to power your Claude Code usage\n\n\
            Current session: 30% used · resets Nov 22, 8am (Asia/Seoul)\n\
            Current week (all models): 61% used · resets Nov 22, 10am (Asia/Seoul)\n\
            Current week (Fable): 0% used · resets Nov 22, 10am (Asia/Seoul)\n\n\
            What's contributing to your limits usage?\n  98% of your usage was at >150k context\n";
        let f = parse(text, sent).unwrap();
        let seoul = |h| crate::time::to_ts_in(2026, 11, 22, h, 0, &crate::time::tz_named("Asia/Seoul").unwrap());
        assert_eq!(f.five_hour, Some(BridgeWindow { used_percentage: 30.0, resets_at: seoul(8) }));
        assert_eq!(f.seven_day, Some(BridgeWindow { used_percentage: 61.0, resets_at: seoul(10) }));
        assert_eq!(f.models.len(), 1);
        assert_eq!(f.models[0].name, "Fable");
        assert_eq!(f.models[0].window.used_percentage, 0.0);
        assert_eq!(f.weekly_anchor, seoul(10));
    }

    #[test]
    fn ignores_unrelated_text() {
        assert!(parse("/usage isn't available in this environment.", 0.0).is_none());
    }
}
