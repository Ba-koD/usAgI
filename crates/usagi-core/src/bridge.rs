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
    /// The usage source omitted the all-model weekly quota, so its value is unknown.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub weekly_unavailable: bool,
    /// 마지막으로 공급자가 알려준 주간 리셋 시각.
    #[serde(default)]
    pub weekly_anchor: Option<f64>,
    /// 모델별 주간 한도 (예: Fable). 직접 조회 또는 `/usage`에서 받음
    #[serde(default)]
    pub models: Vec<ModelWindow>,
    /// 지출 한도 (상태 표시줄 rate_limits.spend_limit, 지출 한도를 정한 환경에서만 옴)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spend: Option<Spend>,
    /// 사용량 조회의 추가 사용량·크레딧 안내
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<String>,
    /// Claude 쿠폰, 만료가 빠른 순. None은 이번 기록에서 확인하지 않음 (저장할 때 예전 값을 이어 씀)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coupons: Option<Vec<Coupon>>,
    /// Claude 사용 크레딧 잔액·자동 충전. 쿠폰과 같은 조회에서 옴 (쿠폰을 확인하지 않은 기록은 예전 값을 이어 씀)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub balance: Option<Balance>,
    /// Claude 리필권 (한도 초기화권, Claude Code `/limit-reset`). None은 이번 기록에 없음 (예전 값을 이어 씀)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refills: Option<crate::codex::Refills>,
    /// 사용량 조회에서 온 기록인지 (저장하지 않음). 상태 표시줄에는 추가 사용량 안내가 없음
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
        let finite = |key| v.get(key).and_then(|r| r.as_f64()).filter(|n| n.is_finite());
        Some(Spend {
            used_percentage: finite("used_percentage")?,
            resets_at: v.get("resets_at").and_then(|r| r.as_f64()).and_then(time::timestamp),
            used_usd: finite("used_usd"),
            limit_usd: finite("limit_usd"),
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

/// Claude 쿠폰: 사용 크레딧 잔액 중 만료일이 있는 프로모션 크레딧.
/// Claude Code `/usage-credits`가 보여 주는 promo_tranches와 같은 값
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Coupon {
    /// 남은 금액 (통화 기본 단위, 50.0 = $50)
    pub amount: f64,
    /// ISO 통화 코드 (USD 등)
    pub currency: String,
    pub expires_at: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Coupon {
    pub fn amount_label(&self) -> String {
        money_label(self.amount, &self.currency)
    }
}

/// Claude 사용 크레딧 잔액과 자동 충전 설정.
/// Claude Code 추가 사용량 메뉴의 "$12.34 balance · auto-reload on"과 같은 값 (prepaid/credits)
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Balance {
    /// 잔액 (통화 기본 단위, 쿠폰 금액 포함)
    pub amount: f64,
    pub currency: String,
    /// 자동 충전 켜짐. 응답에 설정이 없으면 None
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_reload: Option<bool>,
}

impl Balance {
    pub fn amount_label(&self) -> String {
        money_label(self.amount, &self.currency)
    }
}

/// 금액 표시: "$50", "$12.50", "₩70,000", 기호가 없는 통화는 "50 CHF"
pub fn money_label(amount: f64, currency: &str) -> String {
    let cents = (amount * 100.0).round() as i64;
    let whole = group_thousands(cents / 100);
    let number = if cents % 100 == 0 { whole } else { format!("{whole}.{:02}", cents % 100) };
    match currency {
        "USD" => format!("${number}"),
        "EUR" => format!("€{number}"),
        "GBP" => format!("£{number}"),
        "JPY" => format!("¥{number}"),
        "KRW" => format!("₩{number}"),
        code => format!("{number} {code}"),
    }
}

/// 서버 금액(최소 단위) → 기본 단위 배수. 원·엔처럼 소수 단위가 없는 통화는 1
pub fn minor_per_unit(currency: &str) -> f64 {
    if matches!(currency, "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "PYG" | "UGX" | "XAF" | "XOF") {
        1.0
    } else {
        100.0
    }
}

fn group_thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelWindow {
    pub name: String,
    pub window: BridgeWindow,
}

impl BridgeFile {
    /// Report only a reset timestamp supplied by the provider.
    pub fn weekly_reset(&self, now: f64) -> Option<f64> {
        self.seven_day
            .and_then(|w| w.live_reset(now))
            .or_else(|| self.weekly_anchor.filter(|&at| at > now))
    }
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
        used_percentage: v.get("used_percentage")?.as_f64().filter(|p| p.is_finite())?,
        resets_at: v.get("resets_at").and_then(|r| r.as_f64()).and_then(time::timestamp),
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
pub fn save(account_id: &str, mut file: BridgeFile) -> std::io::Result<()> {
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
    // 쿠폰은 가끔만 조회하고 상태 표시줄·CLI `/usage`에는 없음 → 확인하지 않은 기록은 예전 값 중 남은 것을 이어 씀
    if file.coupons.is_none() {
        let now = file.updated_at;
        file.coupons = old.as_ref().and_then(|o| o.coupons.clone()).map(|list| list.into_iter().filter(|c| c.expires_at > now).collect());
        // 잔액도 같은 조회에서만 옴
        file.balance = old.as_ref().and_then(|o| o.balance.clone());
    }
    // 리필권은 HTTPS 사용량 조회에만 옴 → 다른 기록은 만료 전까지 예전 값을 이어 씀
    if file.refills.is_none() {
        let now = file.updated_at;
        file.refills = old.as_ref().and_then(|o| o.refills.clone()).filter(|r| r.expires_at.is_none_or(|at| at > now));
    }
    // 상태 표시줄은 모델별 창을 모르므로, 마지막 `/usage` 값을 리셋 전까지 이어 씀
    if file.models.is_empty() {
        file.models = old
            .map(|o| o.models.into_iter().filter(|m| m.window.resets_at.is_none_or(|r| r > file.updated_at)).collect())
            .unwrap_or_default();
    }
    crate::config::write_json(&path(account_id), &file)
}

/// `usagi statusline --account <id>` 진입점: stdin을 받아 파일에 쓰고 출력할 글자를 돌려줍니다.
pub fn run_statusline(input: &str, account_id: &str) -> String {
    // Claude Code가 따로 띄우는 프로세스라 저장된 언어 설정을 여기서 읽음
    let state = crate::config::State::load();
    state.apply_locale();
    let now = time::now();
    let (file, line) = parse_statusline(input, now);
    if let Some(file) = file {
        if crate::accounts::load(&state).iter().any(|a| a.is_claude() && a.usage_key() == account_id) {
            if let Err(error) = save(account_id, file) {
                eprintln!("usAgI: {error}");
            }
        }
    }
    line
}

/// `statusline` 명령의 계정 (`--account <id>`, 없으면 첫 Claude 계정)
pub fn statusline_account(args: &[String]) -> String {
    args.iter()
        .position(|a| a == "--account")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| "claude-1".into())
}

/// `usagi statusline --account <id>` 명령 전체: stdin JSON으로 사용량을 기록하고 상태 표시줄 글자를 stdout에 씀.
/// 원래 쓰던 상태 표시줄(계정의 `chain`)이 있으면 `run_chain(명령, 입력)`의 출력을 대신 보여 줌.
/// 그 명령을 실행할 셸은 플랫폼마다 달라서 앱이 정함 (macOS는 [`run_chain_sh`]).
pub fn statusline_main(args: &[String], run_chain: impl FnOnce(&str, &str) -> Option<String>) {
    use std::io::{Read, Write};
    let account = statusline_account(args);
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let line = run_statusline(&input, &account);
    let chain = crate::accounts::load(&crate::config::State::load())
        .into_iter()
        .find(|a| a.usage_key() == account)
        .and_then(|a| a.chain);
    // 원래 상태 표시줄이 다시 usAgI를 부른 경우: 체인을 또 실행하면 끝없이 이어짐
    let nested = std::env::var_os(crate::login::STATUSLINE_CHAIN_ENV).is_some();
    let out = statusline_reply(line, chain.as_deref().filter(|_| !nested), &input, run_chain);
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(out.as_bytes());
    let _ = stdout.flush();
}

/// 원래 상태 표시줄이 출력하면 그 글자, 없거나 실패하면 usAgI 글자
fn statusline_reply(line: String, chain: Option<&str>, input: &str, run_chain: impl FnOnce(&str, &str) -> Option<String>) -> String {
    chain.and_then(|cmd| run_chain(cmd, input)).unwrap_or(line)
}

/// 원래 상태 표시줄을 `/bin/sh -c`로 실행 (macOS·Linux)
#[cfg(unix)]
pub fn run_chain_sh(cmd: &str, input: &str) -> Option<String> {
    let mut command = std::process::Command::new("/bin/sh");
    command.arg("-c").arg(cmd);
    crate::login::run_statusline_chain(command, input)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn statusline_account_reads_the_flag() {
        assert_eq!(statusline_account(&args(&["usagi", "statusline", "--account", "claude-3"])), "claude-3");
        assert_eq!(statusline_account(&args(&["usagi", "statusline"])), "claude-1");
        assert_eq!(statusline_account(&args(&["usagi", "statusline", "--account"])), "claude-1", "값이 빠지면 기본 계정");
    }

    #[test]
    fn statusline_reply_prefers_the_original_status_line() {
        let line = || "usAgI".to_string();
        let echo = |cmd: &str, input: &str| Some(format!("{cmd}:{input}"));
        assert_eq!(statusline_reply(line(), Some("orig"), "{}", echo), "orig:{}");
        assert_eq!(statusline_reply(line(), None, "{}", |_: &str, _: &str| -> Option<String> { panic!("체인 없으면 실행 안 함") }), "usAgI");
        assert_eq!(statusline_reply(line(), Some("broken"), "{}", |_: &str, _: &str| None), "usAgI", "실패하면 usAgI 글자");
    }

    #[cfg(unix)]
    #[test]
    fn run_chain_sh_passes_stdin_and_rejects_failures() {
        assert_eq!(run_chain_sh("cat", "hello").as_deref(), Some("hello"));
        assert_eq!(run_chain_sh("printf original", "").as_deref(), Some("original"));
        assert_eq!(run_chain_sh("exit 3", ""), None);
    }

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
    fn absurd_reset_times_are_clamped_to_a_sane_window() {
        let input = r#"{"rate_limits":{"five_hour":{"used_percentage":10,"resets_at":1e300},
            "seven_day":{"used_percentage":20,"resets_at":-1e300},
            "spend_limit":{"used_percentage":5,"resets_at":1e300}}}"#;
        let (file, _) = parse_statusline(input, 1_900_000_000.0);
        let file = file.unwrap();
        assert_eq!(file.five_hour.unwrap().resets_at, Some(time::MAX_TIMESTAMP));
        assert_eq!(file.seven_day.unwrap().resets_at, Some(0.0));
        assert_eq!(file.spend.unwrap().resets_at, Some(time::MAX_TIMESTAMP));
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
    fn coupon_amounts_use_the_currency_symbol() {
        let c = |amount: f64, currency: &str| Coupon { amount, currency: currency.into(), expires_at: 0.0, name: None };
        assert_eq!(c(50.0, "USD").amount_label(), "$50");
        assert_eq!(c(12.5, "USD").amount_label(), "$12.50");
        assert_eq!(c(70_000.0, "KRW").amount_label(), "₩70,000");
        assert_eq!(c(1_234.5, "CHF").amount_label(), "1,234.50 CHF");
        assert_eq!(minor_per_unit("USD"), 100.0);
        assert_eq!(minor_per_unit("KRW"), 1.0);
    }

    #[test]
    fn unchecked_records_keep_unexpired_coupons() {
        let id = "coupon-carry-test";
        let coupon = |expires_at: f64| Coupon { amount: 5.0, currency: "USD".into(), expires_at, name: None };
        let balance = Balance { amount: 62.5, currency: "USD".into(), auto_reload: Some(true) };
        let first = BridgeFile {
            updated_at: 100.0,
            coupons: Some(vec![coupon(150.0), coupon(500.0)]),
            balance: Some(balance.clone()),
            from_usage: true,
            ..Default::default()
        };
        save(id, first).unwrap();
        // 상태 표시줄·CLI 기록에는 쿠폰·잔액이 없음 → 남은 쿠폰과 잔액을 이어 씀
        save(id, BridgeFile { updated_at: 200.0, ..Default::default() }).unwrap();
        assert_eq!(read(id).unwrap().coupons, Some(vec![coupon(500.0)]));
        assert_eq!(read(id).unwrap().balance, Some(balance));
        // 확인했는데 없으면 비움
        save(id, BridgeFile { updated_at: 300.0, coupons: Some(Vec::new()), from_usage: true, ..Default::default() }).unwrap();
        assert_eq!(read(id).unwrap().coupons, Some(Vec::new()));
        assert_eq!(read(id).unwrap().balance, None);
        let _ = std::fs::remove_file(path(id));
    }

    #[test]
    fn unchecked_records_keep_refills_until_they_expire() {
        let id = "refill-carry-test";
        let refills = crate::codex::Refills { available: 1, expires_at: Some(250.0) };
        let first = BridgeFile { updated_at: 100.0, refills: Some(refills.clone()), from_usage: true, ..Default::default() };
        save(id, first).unwrap();
        save(id, BridgeFile { updated_at: 200.0, ..Default::default() }).unwrap();
        assert_eq!(read(id).unwrap().refills, Some(refills));
        save(id, BridgeFile { updated_at: 300.0, ..Default::default() }).unwrap();
        assert_eq!(read(id).unwrap().refills, None, "기한이 지나면 이어 쓰지 않음");
        let _ = std::fs::remove_file(path(id));
    }

    #[test]
    fn weekly_reset_waits_for_new_provider_timestamp() {
        let b = BridgeFile { updated_at: 0.0, weekly_anchor: Some(1000.0), ..Default::default() };
        assert_eq!(b.weekly_reset(999.0), Some(1000.0));
        assert_eq!(b.weekly_reset(1010.0), None);
    }
}
