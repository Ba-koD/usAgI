//! Read Claude quota with the access token in the selected Claude Code profile.
use crate::accounts::Account;
use crate::bridge::{BridgeFile, BridgeWindow, Coupon, ModelWindow};
use crate::codex::Refills;
use crate::usage_http::{self, read_bounded, RequestError, CLIENT};
use crate::usage_retry::UsageError;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

/// cedar_ember=1: 리필권(한도 초기화권) 상태를 같은 응답에 함께 받음 (Claude Code `/limit-reset`과 같은 표시)
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage?cedar_ember=1";
/// 쿠폰(만료일 있는 프로모션 크레딧) 조회 주소의 앞부분. Claude Code `/usage-credits`와 같은 경로
const API_BASE: &str = "https://api.anthropic.com";
/// 쿠폰은 자주 바뀌지 않으므로 계정마다 이 간격으로만 조회 (로그인이 바뀌면 바로)
const COUPON_INTERVAL_SECS: f64 = 60.0 * 60.0;
const BETA: &[(&str, &str)] = &[("anthropic-beta", "oauth-2025-04-20")];
const EXPIRY_LEEWAY_SECS: f64 = 60.0;
const AUTH_OVERRIDES: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "CLAUDE_CODE_USE_FOUNDRY",
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Credentials {
    claude_ai_oauth: Option<Session>,
}

#[derive(Default)]
struct ProfileState {
    rejected: Option<(u64, f64)>,
}

impl ProfileState {
    fn accepts(&mut self, session: &Session) -> bool {
        if let Some((fingerprint, _)) = self.rejected {
            if fingerprint == FINGERPRINTS.hash_one(&session.access_token) {
                return false;
            }
            self.rejected = None;
        }
        true
    }
}

static PROFILES: LazyLock<Mutex<HashMap<PathBuf, ProfileState>>> = LazyLock::new(Mutex::default);
static FINGERPRINTS: LazyLock<RandomState> = LazyLock::new(RandomState::new);
/// 프로필 폴더별 마지막 쿠폰 조회 (로그인 지문, 시각)
static COUPON_CHECKS: LazyLock<Mutex<HashMap<PathBuf, (u64, f64)>>> = LazyLock::new(Mutex::default);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Session {
    access_token: String,
    expires_at: f64,
    scopes: Vec<String>,
    pub(crate) rate_limit_tier: Option<String>,
    pub(crate) subscription_type: Option<String>,
    pub(crate) refresh_token_expires_at: Option<f64>,
}

impl Session {
    pub(crate) fn fingerprint(&self) -> u64 {
        FINGERPRINTS.hash_one(&self.access_token)
    }
}

fn env_is_set(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|value| !value.is_empty() && value != "0" && value != "false")
}

fn custom_backend(url: &str) -> bool {
    !url.is_empty() && url.trim_end_matches('/') != "https://api.anthropic.com"
}

fn uses_file_login(dir: &Path) -> bool {
    if AUTH_OVERRIDES.iter().any(|key| env_is_set(key))
        || std::env::var("ANTHROPIC_BASE_URL").is_ok_and(|url| custom_backend(&url))
    {
        return false;
    }
    for name in ["settings.json", "settings.local.json"] {
        let bytes = match read_bounded(&dir.join(name)) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => return false,
        };
        let Ok(settings) = serde_json::from_slice::<Value>(&bytes) else {
            return false;
        };
        let env = &settings["env"];
        if settings["apiKeyHelper"]
            .as_str()
            .is_some_and(|helper| !helper.is_empty())
            || AUTH_OVERRIDES.iter().any(|key| {
                env[*key]
                    .as_str()
                    .is_some_and(|value| !matches!(value, "" | "0" | "false"))
            })
            || env["ANTHROPIC_BASE_URL"]
                .as_str()
                .is_some_and(custom_backend)
        {
            return false;
        }
    }
    true
}

fn load_session(dir: &Path, now: f64) -> Option<Session> {
    if dir.as_os_str().is_empty() {
        return None;
    }
    let bytes = read_bounded(&dir.join(".credentials.json")).ok()?;
    let session = serde_json::from_slice::<Credentials>(&bytes)
        .ok()?
        .claude_ai_oauth?;
    let token = &session.access_token;
    (session.expires_at > (now + EXPIRY_LEEWAY_SECS) * 1000.0
        && session.scopes.iter().any(|scope| scope == "user:profile")
        && token.starts_with("sk-ant-oat")
        && token
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.')))
    .then_some(session)
}

pub(crate) fn file_session(dir: &Path, now: f64) -> Option<Session> {
    configured_session(dir, now).filter(|session| {
        PROFILES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(dir)
            .is_none_or(|state| state.accepts(session))
    })
}

fn configured_session(dir: &Path, now: f64) -> Option<Session> {
    load_session(dir, now).filter(|_| uses_file_login(dir))
}

pub(crate) fn file_session_rejected(dir: &Path) -> bool {
    PROFILES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(dir)
        .and_then(|state| state.rejected)
        .is_some_and(|(_, expires_at)| expires_at > crate::time::now() * 1000.0)
}

pub(crate) fn available(dir: &Path) -> bool {
    file_session(dir, crate::time::now()).is_some()
}

fn window(value: &Value, key: &str) -> Option<BridgeWindow> {
    let used_percentage = value[key]
        .as_f64()
        .filter(|percent| percent.is_finite() && *percent >= 0.0)?;
    let reset = &value["resets_at"];
    let resets_at = reset
        .as_str()
        .and_then(crate::time::parse_iso)
        .or_else(|| reset.as_f64())
        .and_then(crate::time::timestamp)
        .filter(|reset| *reset > 0.0);
    Some(BridgeWindow {
        used_percentage,
        resets_at,
    })
}

fn extra_usage(value: &Value) -> Option<String> {
    if value["is_enabled"].as_bool() != Some(true) {
        return None;
    }
    let amount = match (
        value["used_credits"].as_f64(),
        value["monthly_limit"].as_f64(),
    ) {
        (Some(used), Some(limit))
            if used >= 0.0
                && limit > 0.0
                && value["currency"]
                    .as_str()
                    .is_none_or(|currency| currency.eq_ignore_ascii_case("USD")) =>
        {
            format!("${:.2} / ${:.2}", used / 100.0, limit / 100.0)
        }
        _ => format!(
            "{:.0}%",
            value["utilization"]
                .as_f64()
                .filter(|percent| *percent >= 0.0)?
        ),
    };
    Some(crate::tr!(
        format!("추가 사용 {amount} · 월"),
        format!("Extra usage {amount} · monthly")
    ))
}

fn parse(value: &Value, now: f64) -> Option<BridgeFile> {
    let five_hour = window(&value["five_hour"], "utilization");
    let limits = value["limits"].as_array().into_iter().flatten();
    let seven_day = window(&value["seven_day"], "utilization").or_else(|| {
        limits
            .clone()
            .filter(|entry| entry["kind"] == "weekly_all")
            .find_map(|entry| window(entry, "percent"))
    });
    let mut seen = std::collections::HashSet::new();
    let mut models: Vec<ModelWindow> = limits
        .filter(|entry| {
            entry["kind"].as_str() == Some("weekly_scoped")
                && entry["group"].as_str() == Some("weekly")
        })
        .filter_map(|entry| {
            let model = &entry["scope"]["model"];
            let name = model["display_name"].as_str()?.trim();
            if name.is_empty()
                || name.len() > 64
                || name.chars().any(char::is_control)
                || name.eq_ignore_ascii_case("all models")
                || model["id"]
                    .as_str()
                    .is_some_and(|id| id.ends_with("all-models") || id.ends_with("all_models"))
            {
                return None;
            }
            let window = window(entry, "percent")?;
            seen.insert(name.to_lowercase()).then(|| ModelWindow {
                name: name.to_owned(),
                window,
            })
        })
        .collect();
    for (key, name) in [
        ("seven_day_fable", "Fable"),
        ("seven_day_sonnet", "Sonnet"),
        ("seven_day_opus", "Opus"),
    ] {
        if !models
            .iter()
            .any(|model| model.name.eq_ignore_ascii_case(name))
        {
            if let Some(window) = window(&value[key], "utilization") {
                models.push(ModelWindow {
                    name: name.into(),
                    window,
                });
            }
        }
    }
    let extra = extra_usage(&value["extra_usage"]);
    let refills = limit_resets(&value["cedar_ember"], now);
    if five_hour.is_none() && seven_day.is_none() && models.is_empty() {
        return None;
    }
    Some(BridgeFile {
        updated_at: now,
        five_hour,
        seven_day,
        weekly_unavailable: seven_day.is_none(),
        weekly_anchor: seven_day.and_then(|window| window.resets_at),
        models,
        extra,
        refills,
        from_usage: true,
        ..Default::default()
    })
}

/// 리필권 (응답의 cedar_ember.grants). 받은 적이 없거나 모두 기한이 지났으면 None.
/// 남은 장수는 기한 안의 것만 더하고, 만료는 남은 것 중 가장 빠른 기한
fn limit_resets(value: &Value, now: f64) -> Option<Refills> {
    let live: Vec<(u32, Option<f64>)> = value["grants"]
        .as_array()?
        .iter()
        .filter_map(|grant| {
            let left = u32::try_from(grant["resets_left"].as_u64()?).ok()?;
            let ends_at = grant["ends_at"].as_str().and_then(crate::time::parse_iso);
            ends_at.is_none_or(|at| at > now).then_some((left, ends_at))
        })
        .collect();
    if live.is_empty() {
        return None;
    }
    Some(Refills {
        available: live.iter().fold(0u32, |sum, (left, _)| sum.saturating_add(*left)),
        expires_at: live
            .iter()
            .filter(|(left, _)| *left > 0)
            .filter_map(|(_, at)| *at)
            .min_by(f64::total_cmp),
    })
}

pub(crate) fn read(acc: &Account, approved_fingerprint: Option<u64>) -> Result<Option<BridgeFile>, UsageError> {
    let dir = acc.path();
    let session = configured_session(&dir, crate::time::now());
    if approved_fingerprint.is_some() && session.as_ref().map(Session::fingerprint) != approved_fingerprint {
        return Err(UsageError::ProfileChanged);
    }
    let mut usage = read_with(&CLIENT, USAGE_URL, &dir, session.as_ref())?;
    // 사용량을 받은 로그인으로만 쿠폰 확인. 실패해도 사용량에는 영향 없음
    if let (Some(usage), Some(session)) = (usage.as_mut(), session.as_ref()) {
        // .claude.json은 클 수 있어 쿠폰을 조회할 때만 읽음
        usage.coupons = coupons(&CLIENT, API_BASE, &dir, || crate::login::claude_org_uuid(&dir), session, crate::time::now());
    }
    Ok(usage)
}

/// 조직 ID는 주소에 들어가므로 UUID 모양만 받음
fn valid_org(org: &str) -> bool {
    org.len() == 36 && org.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-')
}

/// 쿠폰 조회 시점이면 조회. 건너뛰었거나 실패하면 None (저장된 값을 이어 씀)
fn coupons(
    client: &ureq::Agent,
    base: &str,
    dir: &Path,
    org: impl FnOnce() -> Option<String>,
    session: &Session,
    now: f64,
) -> Option<Vec<Coupon>> {
    {
        let fingerprint = session.fingerprint();
        let mut checks = COUPON_CHECKS.lock().unwrap_or_else(|e| e.into_inner());
        if checks
            .get(dir)
            .is_some_and(|&(seen, at)| seen == fingerprint && now - at < COUPON_INTERVAL_SECS)
        {
            return None;
        }
        checks.insert(dir.to_owned(), (fingerprint, now));
    }
    let org = org().filter(|org| valid_org(org))?;
    let url = format!("{base}/api/oauth/organizations/{org}/prepaid/credits");
    let headers = [BETA[0], ("x-organization-uuid", org.as_str())];
    let value = usage_http::get_json(client, &url, &session.access_token, &headers).ok()?;
    Some(parse_coupons(&value, now))
}

/// promo_tranches → 남은 금액이 있고 아직 만료되지 않은 쿠폰, 만료가 빠른 순
fn parse_coupons(value: &Value, now: f64) -> Vec<Coupon> {
    let mut list: Vec<Coupon> = value["promo_tranches"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|tranche| {
            let currency = tranche["currency"].as_str()?.trim().to_ascii_uppercase();
            if currency.len() != 3 || !currency.bytes().all(|c| c.is_ascii_alphabetic()) {
                return None;
            }
            let minor = tranche["remaining_amount_minor_units"]
                .as_f64()
                .filter(|amount| amount.is_finite() && *amount > 0.0)?;
            let expires_at = tranche["expires_at"]
                .as_str()
                .and_then(crate::time::parse_iso)
                .filter(|&at| at > now)?;
            let name = tranche["name"]
                .as_str()
                .map(str::trim)
                .filter(|name| !name.is_empty() && name.chars().count() <= 64 && !name.chars().any(char::is_control))
                .map(str::to_owned);
            Some(Coupon {
                amount: minor / crate::bridge::minor_per_unit(&currency),
                currency,
                expires_at,
                name,
            })
        })
        .take(16)
        .collect();
    list.sort_by(|a, b| a.expires_at.total_cmp(&b.expires_at));
    list
}

fn read_with(
    client: &ureq::Agent,
    url: &str,
    dir: &Path,
    session: Option<&Session>,
) -> Result<Option<BridgeFile>, UsageError> {
    let now = crate::time::now();
    let Some(session) = session else {
        return Ok(None);
    };
    {
        let mut profiles = PROFILES.lock().unwrap_or_else(|e| e.into_inner());
        profiles.retain(|_, state| {
            state
                .rejected
                .is_some_and(|(_, expiry)| expiry > now * 1000.0)
        });
        if profiles
            .get_mut(dir)
            .is_some_and(|state| !state.accepts(session))
        {
            return Ok(None);
        }
    }
    match fetch(client, url, session) {
        Ok(Some(usage)) => Ok(Some(usage)),
        Ok(None) => Err(UsageError::Failed(
            crate::tr!("사용량 정보 없음", "No usage info").into(),
        )),
        Err(RequestError::AuthRejected) => {
            let fingerprint = session.fingerprint();
            let mut profiles = PROFILES.lock().unwrap_or_else(|e| e.into_inner());
            let state = profiles.entry(dir.to_owned()).or_default();
            state.rejected = Some((fingerprint, session.expires_at));
            Ok(None)
        }
        Err(RequestError::RateLimited(delay)) => Err(UsageError::rate_limited(delay)),
        Err(RequestError::Server) => Err(UsageError::Failed(
            crate::tr!(
                "Claude 사용량 서버 응답 없음",
                "The Claude usage server is temporarily unavailable"
            )
            .into(),
        )),
        Err(RequestError::Unavailable) => Err(UsageError::Failed(
            crate::tr!(
                "Claude 사용량 조회 실패",
                "Could not fetch Claude usage"
            )
            .into(),
        )),
    }
}

fn fetch(
    client: &ureq::Agent,
    url: &str,
    session: &Session,
) -> Result<Option<BridgeFile>, RequestError> {
    let value = usage_http::get_json(client, url, &session.access_token, BETA)?;
    Ok(parse(&value, crate::time::now()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    fn test_session(token: &str) -> Session {
        Session {
            access_token: token.into(),
            expires_at: (crate::time::now() + 600.0) * 1000.0,
            scopes: vec!["user:profile".into()],
            rate_limit_tier: None,
            subscription_type: None,
            refresh_token_expires_at: None,
        }
    }

    fn test_client() -> ureq::Agent {
        ureq::Agent::config_builder()
            .proxy(None)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(3)))
            .build()
            .new_agent()
    }

    fn status_response(status: &str, headers: &str) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/usage", listener.local_addr().unwrap());
        let response =
            format!("HTTP/1.1 {status}\r\n{headers}Content-Length: 0\r\nConnection: close\r\n\r\n");
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut request = String::new();
            while !request.ends_with("\r\n\r\n") {
                assert_ne!(reader.read_line(&mut request).unwrap(), 0);
            }
            stream.write_all(response.as_bytes()).unwrap();
        });
        (url, server)
    }

    #[test]
    fn reads_only_unexpired_file_sessions_with_profile_scope() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(".credentials.json");
        let mut data = json!({
            "claudeAiOauth": {
                "accessToken": "sk-ant-oat01-fixture",
                "refreshToken": "unused-fixture",
                "expiresAt": 500_000,
                "scopes": ["user:profile", "user:inference"]
            }
        });
        let initial = serde_json::to_vec(&data).unwrap();
        std::fs::write(&file, &initial).unwrap();
        assert!(load_session(dir.path(), 100.0).is_some());
        assert!(load_session(dir.path(), 439.0).is_some());
        assert!(load_session(dir.path(), 440.0).is_none());
        assert!(load_session(dir.path(), 500.0).is_none());
        assert_eq!(std::fs::read(&file).unwrap(), initial);
        data["claudeAiOauth"]["scopes"] = json!(["user:inference"]);
        std::fs::write(&file, serde_json::to_vec(&data).unwrap()).unwrap();
        assert!(load_session(dir.path(), 100.0).is_none());
        assert!(load_session(&dir.path().join("other-profile"), 100.0).is_none());
    }

    #[test]
    fn rejected_sessions_take_cli_path_until_the_token_changes() {
        let client = test_client();
        for status in ["401 Unauthorized", "403 Forbidden"] {
            let dir = tempfile::tempdir().unwrap();
            let (url, server) = status_response(status, "");
            let rejected = || Some(test_session("sk-ant-oat01-rejected"));
            assert!(read_with(&client, &url, dir.path(), rejected().as_ref())
                .unwrap()
                .is_none());
            server.join().unwrap();
            assert!(file_session_rejected(dir.path()));
            // The server has exited. The same token still falls straight through to the CLI.
            assert!(read_with(&client, &url, dir.path(), rejected().as_ref())
                .unwrap()
                .is_none());
            let mut profiles = PROFILES.lock().unwrap();
            assert!(profiles
                .get_mut(dir.path())
                .unwrap()
                .accepts(&test_session("sk-ant-oat01-rotated")));
        }
    }

    #[test]
    fn rate_limit_blocks_http_and_cli_even_after_the_file_expires() {
        let client = test_client();
        for (headers, expected) in [("Retry-After: 120\r\n", 120), ("", 300)] {
            let dir = tempfile::tempdir().unwrap();
            let (url, server) = status_response("429 Too Many Requests", headers);
            let id = dir.path().to_string_lossy();
            let before = crate::time::now();
            let session = Some(test_session("sk-ant-oat01-rate-limited"));
            let result =
                crate::usage_retry::run(&id, || read_with(&client, &url, dir.path(), session.as_ref()));
            server.join().unwrap();
            let retry = crate::usage_retry::deadline(&id).unwrap();
            assert_eq!(result.unwrap_err(), UsageError::RateLimited(retry));
            let expected = f64::from(expected);
            assert!(retry >= before + expected && retry <= crate::time::now() + expected);
            assert_eq!(
                crate::usage_retry::run(&id, || read_with(&client, &url, dir.path(), None))
                    .unwrap_err(),
                UsageError::Deferred
            );
        }
    }

    #[test]
    fn coupons_are_read_with_the_org_header_at_most_hourly() {
        const ORG: &str = "0f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut request = String::new();
            while !request.ends_with("\r\n\r\n") {
                assert_ne!(reader.read_line(&mut request).unwrap(), 0);
            }
            let request = request.to_ascii_lowercase();
            assert!(request.starts_with(&format!(
                "get /api/oauth/organizations/{ORG}/prepaid/credits http/1.1"
            )));
            assert!(request.contains("\r\nauthorization: bearer sk-ant-oat01-fixture\r\n"));
            assert!(request.contains(&format!("\r\nx-organization-uuid: {ORG}\r\n")));
            assert!(request.contains("\r\nanthropic-beta: oauth-2025-04-20\r\n"));
            let response = json!({
                "amount": 7000, "currency": "USD",
                "promo_tranches": [
                    {"remaining_amount_minor_units": 5000, "currency": "usd", "expires_at": "2026-12-20T00:00:00Z", "name": "Launch credit"},
                    {"remaining_amount_minor_units": 70000, "currency": "KRW", "expires_at": "2026-12-05T00:00:00Z"},
                    {"remaining_amount_minor_units": 1000, "currency": "USD", "expires_at": "2026-11-01T00:00:00Z"},
                    {"remaining_amount_minor_units": 0, "currency": "USD", "expires_at": "2026-12-30T00:00:00Z"},
                    {"remaining_amount_minor_units": 1000, "currency": "US$", "expires_at": "2026-12-30T00:00:00Z"},
                    {"remaining_amount_minor_units": 1000, "currency": "USD"}
                ]
            })
            .to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        });
        let client = test_client();
        let session = test_session("sk-ant-oat01-fixture");
        let dir = tempfile::tempdir().unwrap();
        let now = crate::time::parse_iso("2026-11-25T00:00:00Z").unwrap();
        let list = coupons(&client, &base, dir.path(), || Some(ORG.into()), &session, now).unwrap();
        server.join().unwrap();
        // 만료·0원·잘못된 통화·만료일 없음은 빼고, 만료가 빠른 순
        assert_eq!(list.len(), 2);
        assert_eq!((list[0].amount, list[0].currency.as_str()), (70_000.0, "KRW"));
        assert_eq!((list[1].amount, list[1].currency.as_str()), (50.0, "USD"));
        assert_eq!(list[1].name.as_deref(), Some("Launch credit"));
        // 한 시간 안에는 다시 조회하지 않고 .claude.json도 읽지 않음 (서버는 이미 닫힘)
        let skipped = coupons(&client, &base, dir.path(), || panic!("조직 ID를 읽음"), &session, now + 1800.0);
        assert!(skipped.is_none());
        // 조직 ID가 UUID 모양이 아니거나 없으면 요청하지 않음
        for org in [Some("../usage"), None] {
            let other = tempfile::tempdir().unwrap();
            assert!(coupons(&client, &base, other.path(), || org.map(str::to_owned), &session, now).is_none());
        }
    }

    #[test]
    fn limit_resets_come_from_the_cedar_ember_grants() {
        let now = crate::time::parse_iso("2026-11-25T00:00:00Z").unwrap();
        let block = json!({"eligible": true, "grants": [
            {"id": "g1", "resets_total": 2, "resets_left": 1, "ends_at": "2026-12-10T00:00:00Z"},
            {"id": "g2", "resets_total": 1, "resets_left": 1, "ends_at": "2026-12-03T00:00:00Z"},
            {"id": "g3", "resets_total": 1, "resets_left": 0, "ends_at": "2026-12-01T00:00:00Z"},
            {"id": "old", "resets_total": 1, "resets_left": 1, "ends_at": "2026-11-01T00:00:00Z"},
            {"id": "bad", "resets_left": -1}
        ]});
        // 기한 안의 남은 장수 합계, 만료는 남은 것 중 가장 빠른 기한
        assert_eq!(
            limit_resets(&block, now),
            Some(Refills { available: 2, expires_at: crate::time::parse_iso("2026-12-03T00:00:00Z") })
        );
        // 다 쓴 경우는 0장, 받은 적이 없거나 모두 기한이 지났으면 표시 없음
        let used = json!({"grants": [{"resets_left": 0, "ends_at": "2026-12-10T00:00:00Z"}]});
        assert_eq!(limit_resets(&used, now), Some(Refills { available: 0, expires_at: None }));
        assert!(limit_resets(&json!({"eligible": false, "grants": []}), now).is_none());
        assert!(limit_resets(&Value::Null, now).is_none());
        let expired = json!({"grants": [{"resets_left": 1, "ends_at": "2026-11-01T00:00:00Z"}]});
        assert!(limit_resets(&expired, now).is_none());
    }

    #[test]
    fn accounts_without_promo_credits_have_no_coupons() {
        assert!(parse_coupons(&json!({"amount": null}), 0.0).is_empty());
        assert!(parse_coupons(&json!({"amount": 0, "promo_tranches": []}), 0.0).is_empty());
    }

    #[test]
    fn oauth_apps_is_never_used_as_the_all_model_weekly_limit() {
        let mut response = json!({
            "five_hour": {"utilization": 12},
            "seven_day_oauth_apps": {"utilization": 85}
        });
        let usage = parse(&response, 100.0).unwrap();
        assert!(usage.seven_day.is_none());
        assert!(usage.weekly_unavailable);
        assert_eq!(usage.five_hour.unwrap().used_percentage, 12.0);

        response["limits"] = json!([{"kind":"weekly_all","percent":37}]);
        let usage = parse(&response, 100.0).unwrap();
        assert_eq!(usage.seven_day.unwrap().used_percentage, 37.0);
        assert!(!usage.weekly_unavailable);
    }

    #[test]
    fn one_usage_request_reads_scoped_windows_and_extra_usage() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/api/oauth/usage", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(&mut stream);
            let mut request = String::new();
            while !request.ends_with("\r\n\r\n") {
                assert_ne!(reader.read_line(&mut request).unwrap(), 0);
            }
            let request = request.to_ascii_lowercase();
            assert!(request.starts_with("get /api/oauth/usage http/1.1"));
            assert!(request.contains("\r\nauthorization: bearer sk-ant-oat01-fixture\r\n"));
            assert!(request.contains("\r\nanthropic-beta: oauth-2025-04-20\r\n"));
            let response = json!({
                "five_hour": null, "seven_day": {"utilization":37.5, "resets_at":"2026-11-24T00:00:00Z"},
                "seven_day_opus":{"utilization":20.0},
                "limits":[{"group":"weekly","kind":"weekly_scoped","percent":50,
                    "scope":{"model":{"id":"fable","display_name":"Fable"}}}, false],
                "extra_usage":{"is_enabled":true,"monthly_limit":5000,"used_credits":320,"currency":"USD"},
                "cedar_ember":{"eligible":true,"grants":[{"id":"g1","resets_left":2,"ends_at":"2099-01-01T00:00:00Z"}]}
            }).to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        });
        let client = test_client();
        let session = test_session("sk-ant-oat01-fixture");
        let usage = fetch(&client, &url, &session).unwrap().unwrap();
        server.join().unwrap();
        assert!(usage.five_hour.is_none());
        let weekly = usage.seven_day.unwrap();
        assert_eq!(weekly.used_percentage, 37.5);
        assert_eq!(
            weekly.resets_at,
            crate::time::parse_iso("2026-11-24T00:00:00Z")
        );
        assert_eq!(
            usage
                .models
                .iter()
                .map(|m| m.name.as_str())
                .collect::<Vec<_>>(),
            ["Fable", "Opus"]
        );
        assert!(usage.extra.unwrap().contains("$3.20 / $50.00"));
        assert_eq!(usage.refills.unwrap().available, 2);
        assert!(usage.from_usage);
        assert!(USAGE_URL.ends_with("/api/oauth/usage?cedar_ember=1"));
    }
}
