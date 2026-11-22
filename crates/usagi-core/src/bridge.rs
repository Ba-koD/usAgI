//! Claude Code 상태 표시줄 브리지 — 정확한 Claude 사용량을 로그인 없이 받는 공식 경로.
//!
//! Claude Code는 상태 표시줄 명령의 stdin JSON에 다음을 넣어 줍니다 (Pro/Max/Ultra, 첫 응답 이후):
//!   rate_limits.five_hour.used_percentage (0~100), rate_limits.five_hour.resets_at (epoch 초)
//!   rate_limits.seven_day.used_percentage,       rate_limits.seven_day.resets_at
//! https://code.claude.com/docs/en/statusline.md
//!
//! `usagi statusline` 이 그 JSON을 받아 이 파일에 적고, 상태 표시줄에 짧은 글자를 출력합니다.
//! usAgI 본체는 이 파일만 읽습니다. 토큰이나 로그인 정보는 전혀 다루지 않습니다.

use crate::config::config_dir;
use crate::texts::{fmt_hm, fmt_pct};
use crate::time;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BridgeWindow {
    pub used_percentage: f64,
    pub resets_at: Option<f64>,
}

impl BridgeWindow {
    /// 지금 기준 비율. 리셋이 지났으면 0.
    pub fn ratio_at(&self, now: f64) -> f64 {
        match self.resets_at {
            Some(r) if r <= now => 0.0,
            _ => (self.used_percentage / 100.0).max(0.0),
        }
    }
    pub fn live_reset(&self, now: f64) -> Option<f64> {
        self.resets_at.filter(|&r| r > now)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BridgeFile {
    pub updated_at: f64,
    pub five_hour: Option<BridgeWindow>,
    pub seven_day: Option<BridgeWindow>,
    /// 마지막으로 알았던 주간 리셋 시각. 주간 창은 계정마다 7일 주기로 고정이라
    /// Claude Code가 리셋 뒤 창을 빼더라도 이 값으로 다음 리셋을 계산합니다.
    #[serde(default)]
    pub weekly_anchor: Option<f64>,
    /// 모델별 주간 한도 (예: Fable). `/usage`에서만 옴
    #[serde(default)]
    pub models: Vec<ModelWindow>,
    /// 지출 한도 (상태 표시줄 rate_limits.spend_limit, 지출 한도를 정한 환경에서만 옴)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend: Option<Spend>,
    /// `/usage`의 추가 사용량·크레딧 줄 (형식이 공개되지 않아 글자 그대로)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<String>,
    /// `/usage`에서 온 기록인지 (저장하지 않음). 추가 사용량 줄은 `/usage`만 알려 줌
    #[serde(skip)]
    pub from_usage: bool,
}

/// 지출 한도 (금액은 Claude Code가 토큰 수로 추정한 값)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spend {
    pub used_percentage: f64,
    #[serde(default)]
    pub resets_at: Option<f64>,
    #[serde(default)]
    pub used_usd: Option<f64>,
    #[serde(default)]
    pub limit_usd: Option<f64>,
    /// daily | weekly | monthly
    #[serde(default)]
    pub period: Option<String>,
}

impl Spend {
    fn from_json(v: &serde_json::Value) -> Option<Self> {
        Some(Spend {
            used_percentage: v.get("used_percentage")?.as_f64()?,
            resets_at: v.get("resets_at").and_then(|r| r.as_f64()),
            used_usd: v.get("used_usd").and_then(|r| r.as_f64()),
            limit_usd: v.get("limit_usd").and_then(|r| r.as_f64()),
            period: v.get("period").and_then(|r| r.as_str()).map(str::to_string),
        })
    }

    /// 계정 카드 표시: "추가 사용 $12.30 / $50 · 월" (금액이 없으면 퍼센트)
    pub fn label(&self) -> String {
        let period = match self.period.as_deref() {
            Some("daily") => crate::tr!(" · 일", " · daily"),
            Some("weekly") => crate::tr!(" · 주", " · weekly"),
            Some("monthly") => crate::tr!(" · 월", " · monthly"),
            _ => "",
        };
        let amount = match (self.used_usd, self.limit_usd) {
            (Some(u), Some(l)) => format!("${u:.2} / ${l:.0}"),
            _ => format!("{:.0}%", self.used_percentage),
        };
        crate::tr!(format!("추가 사용 {amount}{period}"), format!("Extra usage {amount}{period}"))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelWindow {
    pub name: String,
    pub window: BridgeWindow,
}

impl BridgeFile {
    /// 다음 주간 리셋: 지금 값이 살아 있으면 그것, 아니면 기억한 기준점에서 7일씩 넘김
    pub fn weekly_reset(&self, now: f64) -> Option<f64> {
        self.seven_day
            .and_then(|w| w.live_reset(now))
            .or_else(|| self.weekly_anchor.or(self.seven_day.and_then(|w| w.resets_at)).map(|a| roll_weekly(a, now)))
    }
}

/// 지난 주간 리셋 시각을 7일씩 넘겨 지금 이후의 다음 리셋으로
pub fn roll_weekly(anchor: f64, now: f64) -> f64 {
    const WEEK: f64 = 7.0 * 86400.0;
    if anchor > now {
        return anchor;
    }
    anchor + ((now - anchor) / WEEK).floor() * WEEK + WEEK
}

/// 계정별 기록 파일
pub fn path(account_id: &str) -> PathBuf {
    config_dir().join(format!("claude-usage-{}.json", safe_id(account_id)))
}

/// 파일 이름에 쓸 계정 ID (영문·숫자·-·_ 만). 명령줄로 받은 값이 경로를 벗어나지 않게
fn safe_id(account_id: &str) -> String {
    account_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

pub fn read(account_id: &str) -> Option<BridgeFile> {
    let text = std::fs::read_to_string(path(account_id)).ok()?;
    serde_json::from_str(&text).ok()
}

fn window(v: &serde_json::Value) -> Option<BridgeWindow> {
    Some(BridgeWindow {
        used_percentage: v.get("used_percentage")?.as_f64()?,
        resets_at: v.get("resets_at").and_then(|r| r.as_f64()),
    })
}

/// 상태 표시줄 stdin → (저장할 내용, 상태 표시줄에 출력할 글자)
pub fn parse_statusline(input: &str, now: f64) -> (Option<BridgeFile>, String) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(input) else {
        return (None, "usAgI".into());
    };
    let model = v
        .get("model")
        .and_then(|m| m.get("display_name"))
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let Some(rl) = v.get("rate_limits").filter(|r| r.is_object()) else {
        // 첫 응답 전이거나 구독 플랜이 아님 → 기존 기록 유지
        return (None, if model.is_empty() { "usAgI".into() } else { model });
    };
    let seven_day = rl.get("seven_day").and_then(window);
    let file = BridgeFile {
        updated_at: now,
        five_hour: rl.get("five_hour").and_then(window),
        weekly_anchor: seven_day.and_then(|w| w.resets_at),
        seven_day,
        spend: rl.get("spend_limit").and_then(Spend::from_json),
        ..Default::default()
    };
    let mut parts = Vec::new();
    if !model.is_empty() {
        parts.push(model);
    }
    if let Some(w) = file.five_hour {
        let pct = fmt_pct(w.ratio_at(now));
        let mut s = crate::tr!(format!("5시간 {pct}"), format!("5-hour {pct}"));
        if let Some(r) = w.live_reset(now) {
            let left = fmt_hm(r - now);
            s += &crate::tr!(format!(" ({left} 뒤 리셋)"), format!(" (resets in {left})"));
        }
        parts.push(s);
    }
    if let Some(w) = file.seven_day {
        let pct = fmt_pct(w.ratio_at(now));
        parts.push(crate::tr!(format!("주간 {pct}"), format!("Weekly {pct}")));
    }
    (Some(file), parts.join(" · "))
}

/// 기록 저장 (임시 파일 → 이름 바꾸기). 주간 기준점이 비면 예전 값을 이어서 보관.
pub fn save(account_id: &str, mut file: BridgeFile) {
    let old = read(account_id);
    if file.weekly_anchor.is_none() {
        file.weekly_anchor = old.as_ref().and_then(|o| o.weekly_anchor);
    }
    // 지출 한도는 상태 표시줄에서만, 추가 사용량 줄은 `/usage`에서만 옴 → 다른 쪽 기록은 이어 씀
    if file.spend.is_none() {
        file.spend = old.as_ref().and_then(|o| o.spend.clone()).filter(|s| s.resets_at.is_none_or(|r| r > file.updated_at));
    }
    if !file.from_usage {
        file.extra = old.as_ref().and_then(|o| o.extra.clone());
    }
    // 상태 표시줄은 모델별 창을 모르므로, 마지막 `/usage` 값을 리셋 전까지 이어 씀
    if file.models.is_empty() {
        file.models = old
            .map(|o| o.models.into_iter().filter(|m| m.window.resets_at.is_none_or(|r| r > file.updated_at)).collect())
            .unwrap_or_default();
    }
    let dir = config_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(text) = serde_json::to_string(&file) {
        let tmp = dir.join(format!("claude-usage.{}.{}.tmp", std::process::id(), safe_id(account_id)));
        if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, path(account_id)).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// `usagi statusline --account <id>` 진입점: stdin을 받아 파일에 쓰고 출력할 글자를 돌려줍니다.
pub fn run_statusline(input: &str, account_id: &str) -> String {
    // Claude Code가 따로 띄우는 프로세스라 저장된 언어 설정을 여기서 읽음
    crate::config::State::load().apply_locale();
    let now = time::now();
    let (file, line) = parse_statusline(input, now);
    if let Some(file) = file {
        save(account_id, file);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_documented_fields() {
        let now = 1_795_000_000.0;
        let input = r#"{"model":{"display_name":"Opus"},"rate_limits":{"five_hour":{"used_percentage":37.4,"resets_at":1795007980},"seven_day":{"used_percentage":30,"resets_at":1795300000}}}"#;
        let (file, line) = parse_statusline(input, now);
        let file = file.unwrap();
        assert_eq!(file.five_hour.unwrap().used_percentage, 37.4);
        assert_eq!(file.seven_day.unwrap().resets_at, Some(1_795_300_000.0));
        assert_eq!(line, "Opus · 5시간 37% (2시간 13분 뒤 리셋) · 주간 30%");
        assert_eq!(file.five_hour.unwrap().ratio_at(1_795_008_000.0), 0.0);
        crate::i18n::set(crate::i18n::Lang::En);
        let (_, line) = parse_statusline(input, now);
        crate::i18n::set(crate::i18n::Lang::Ko);
        assert_eq!(line, "Opus · 5-hour 37% (resets in 2h 13m) · Weekly 30%");
    }

    #[test]
    fn reads_spend_limit_when_present() {
        let input = r#"{"rate_limits":{"five_hour":{"used_percentage":10,"resets_at":2000000000},
            "spend_limit":{"used_percentage":24.6,"resets_at":2000000000,"used_usd":12.3,"limit_usd":50,"period":"monthly"}}}"#;
        let (file, _) = parse_statusline(input, 1_900_000_000.0);
        let spend = file.unwrap().spend.unwrap();
        assert_eq!(spend.label(), "추가 사용 $12.30 / $50 · 월");
        let pct_only = Spend { used_percentage: 37.0, resets_at: None, used_usd: None, limit_usd: None, period: None };
        assert_eq!(pct_only.label(), "추가 사용 37%");
    }

    #[test]
    fn missing_rate_limits_keeps_old_file() {
        let (file, line) = parse_statusline(r#"{"model":{"display_name":"Opus"}}"#, 0.0);
        assert!(file.is_none());
        assert_eq!(line, "Opus");
        let (file, _) = parse_statusline(r#"{"rate_limits":{"seven_day":{"used_percentage":5}}}"#, 0.0);
        let file = file.unwrap();
        assert!(file.five_hour.is_none()); // 리셋돼서 빠진 창
        assert_eq!(file.seven_day.unwrap().resets_at, None);
    }

    #[test]
    fn weekly_reset_rolls_forward() {
        let week = 7.0 * 86400.0;
        assert_eq!(roll_weekly(1000.0, 500.0), 1000.0);
        assert_eq!(roll_weekly(1000.0, 1000.0 + 3.0 * 86400.0), 1000.0 + week);
        assert_eq!(roll_weekly(1000.0, 1000.0 + week + 10.0), 1000.0 + 2.0 * week);
        let b = BridgeFile { updated_at: 0.0, weekly_anchor: Some(1000.0), ..Default::default() };
        assert_eq!(b.weekly_reset(1000.0 + 10.0), Some(1000.0 + week));
    }
}
