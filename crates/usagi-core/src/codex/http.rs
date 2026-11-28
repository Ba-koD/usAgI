//! Read-only use of the active Codex profile's file-backed ChatGPT login.
use super::{CodexSnapshot, CodexWindow, Credits, Refills};
use crate::usage_http::{self, read_bounded, RequestError, CLIENT};
use crate::usage_retry::{self, UsageError};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

const BACKEND: &str = "https://chatgpt.com/backend-api";

#[derive(Deserialize)]
struct AuthFile {
    auth_mode: Option<String>,
    #[serde(rename = "OPENAI_API_KEY")]
    api_key: Option<serde::de::IgnoredAny>,
    tokens: Option<AuthTokens>,
}

#[derive(Deserialize)]
struct AuthTokens {
    access_token: String,
    account_id: Option<String>,
    id_token: Option<String>,
}

struct Session {
    access_token: String,
    expires_at: f64,
    account_id: String,
    email: Option<String>,
}

fn jwt_claims(token: &str) -> Option<Value> {
    let mut parts = token.split('.');
    let (_, payload, _) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || payload.is_empty() {
        return None;
    }
    let mut encoded = payload.replace('-', "+").replace('_', "/");
    encoded.push_str(&"=".repeat((4 - encoded.len() % 4) % 4));
    serde_json::from_slice(&crate::b64::decode(&encoded)?).ok()
}

fn session_from(auth: AuthFile, now: f64) -> Option<Session> {
    if auth
        .auth_mode
        .as_deref()
        .is_some_and(|mode| mode != "chatgpt")
        || (auth.auth_mode.is_none() && auth.api_key.is_some())
    {
        return None;
    }
    let tokens = auth.tokens?;
    let claims = jwt_claims(&tokens.access_token)?;
    let expires_at = claims["exp"].as_f64()?;
    if expires_at <= now + 60.0 {
        return None;
    }
    let id_claims = tokens.id_token.as_deref().and_then(jwt_claims);
    let auth_claim = "https://api.openai.com/auth";
    for claim in std::iter::once(&claims).chain(id_claims.as_ref()) {
        if claim[auth_claim]["chatgpt_account_is_fedramp"].as_bool() == Some(true) {
            return None;
        }
    }
    let account_id = tokens.account_id.or_else(|| {
        id_claims.as_ref()?[auth_claim]["chatgpt_account_id"]
            .as_str()
            .map(str::to_owned)
    })?;
    if !account_id.is_empty()
        && account_id.len() <= 128
        && account_id
            .bytes()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == b'-' || ch == b'_')
    {
        let email = id_claims
            .as_ref()
            .and_then(|claims| claims["email"].as_str())
            .or_else(|| claims["https://api.openai.com/profile"]["email"].as_str())
            .filter(|email| {
                email.len() <= 254 && email.contains('@') && !email.chars().any(char::is_control)
            })
            .map(str::to_owned);
        return Some(Session {
            access_token: tokens.access_token,
            expires_at,
            account_id,
            email,
        });
    }
    None
}

fn load_session(dir: &Path, now: f64) -> Option<Session> {
    if dir.as_os_str().is_empty() {
        return None;
    }
    match read_bounded(&dir.join("config.toml")) {
        Ok(bytes) => {
            let config: toml::Value = toml::from_str(std::str::from_utf8(&bytes).ok()?).ok()?;
            if config
                .get("cli_auth_credentials_store")
                .is_some_and(|store| store.as_str() != Some("file"))
                || config.get("chatgpt_base_url").is_some_and(|base| {
                    !matches!(
                        base.as_str().map(|base| base.trim_end_matches('/')),
                        Some(BACKEND | "https://chat.openai.com/backend-api")
                    )
                })
                || config.get("profile").is_some()
            {
                return None;
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return None,
    }
    let auth = read_bounded(&dir.join("auth.json")).ok()?;
    session_from(serde_json::from_slice(&auth).ok()?, now)
}

pub(super) fn profile_identity(dir: &Path) -> Option<(String, String)> {
    let session = load_session(dir, f64::NEG_INFINITY)?;
    Some((session.account_id, session.email?))
}

#[derive(Default)]
struct RejectedSessions {
    tokens: HashMap<PathBuf, RejectedSession>,
    hasher: RandomState,
}

struct RejectedSession {
    fingerprint: u64,
    expires_at: f64,
    retry_cli_at: f64,
}

impl RejectedSessions {
    fn contains(&mut self, dir: &Path, session: &Session, now: f64) -> bool {
        self.tokens.retain(|_, rejected| rejected.expires_at > now);
        let same = self.tokens.get(dir).is_some_and(|rejected| rejected.fingerprint == self.hasher.hash_one(&session.access_token));
        if !same {
            self.tokens.remove(dir);
        }
        same
    }

    fn reject(&mut self, dir: PathBuf, session: &Session) {
        self.tokens.insert(dir, RejectedSession {
            fingerprint: self.hasher.hash_one(&session.access_token),
            expires_at: session.expires_at,
            retry_cli_at: 0.0,
        });
    }
}

static REJECTED: LazyLock<Mutex<RejectedSessions>> = LazyLock::new(Mutex::default);

pub(super) fn recover_with_cli<T>(dir: &Path, recover: impl FnOnce() -> Result<T, String>) -> Result<T, UsageError> {
    let now = crate::time::now();
    let rejected = load_session(dir, now).and_then(|session| {
        let mut state = REJECTED.lock().unwrap_or_else(|e| e.into_inner());
        state.contains(dir, &session, now).then(|| {
            let rejected = &state.tokens[dir];
            (rejected.fingerprint, rejected.retry_cli_at)
        })
    });
    let auth = crate::tr!("Codex 인증 거부 (401/403)", "Codex authentication rejected (401/403)");
    if rejected.is_some_and(|(_, at)| at > now) {
        return Err(UsageError::Failed(auth.into()));
    }
    recover().map_err(|error| {
        if let Some((fingerprint, _)) = rejected {
            if let Some(session) = REJECTED.lock().unwrap_or_else(|e| e.into_inner()).tokens.get_mut(dir)
                .filter(|session| session.fingerprint == fingerprint)
            {
                session.retry_cli_at = crate::time::now() + 600.0;
            }
            UsageError::Failed(format!("{auth} · {error}"))
        } else {
            UsageError::Failed(error)
        }
    })
}

fn usable_session(dir: &Path, now: f64) -> Option<Session> {
    let session = load_session(dir, now)?;
    let rejected = REJECTED.lock().unwrap_or_else(|e| e.into_inner()).contains(dir, &session, now);
    (!rejected).then_some(session)
}

pub(super) fn login_status(dir: &Path) -> Option<crate::login::LoginStatus> {
    let checked_at = crate::time::now();
    let session = load_session(dir, checked_at)?;
    let rejected = REJECTED.lock().unwrap_or_else(|e| e.into_inner()).contains(dir, &session, checked_at);
    Some(crate::login::LoginStatus {
        logged_in: Some(!rejected),
        email: session.email,
        org: Some("ChatGPT".into()),
        organization_id: Some(session.account_id),
        checked_at,
        ..Default::default()
    })
}

fn get_json(client: &ureq::Agent, url: &str, session: &Session) -> Result<Value, RequestError> {
    usage_http::get_json(
        client,
        url,
        &session.access_token,
        &[("ChatGPT-Account-Id", &session.account_id)],
    )
}

fn refills(value: &Value) -> Option<Refills> {
    let available = u32::try_from(value["available_count"].as_u64()?).ok()?;
    let expires_at = value["credits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|credit| credit["status"].as_str() == Some("available"))
        .filter_map(|credit| {
            let at = &credit["expires_at"];
            at.as_f64().or_else(|| crate::time::parse_iso(at.as_str()?)).and_then(crate::time::timestamp)
        })
        .min_by(f64::total_cmp);
    Some(Refills {
        available,
        expires_at,
    })
}

fn snapshot(value: &Value, session: &Session, now: f64) -> Option<CodexSnapshot> {
    if value["account_id"]
        .as_str()
        .is_some_and(|id| id != session.account_id)
    {
        return None;
    }
    let rate = &value["rate_limit"];
    let mut windows = ["primary_window", "secondary_window"]
        .into_iter()
        .filter_map(|key| {
            let window = &rate[key];
            let seconds = window["limit_window_seconds"].as_u64()?;
            let minutes = u32::try_from(seconds / 60).ok()?;
            let used_percent = window["used_percent"].as_f64().filter(|p| p.is_finite())?;
            if minutes == 0 || used_percent < 0.0 {
                return None;
            }
            let resets_at = window["reset_at"].as_f64().and_then(crate::time::timestamp).filter(|at| *at > 0.0);
            Some(CodexWindow {
                minutes,
                used_percent,
                resets_at,
            })
        })
        .collect::<Vec<_>>();
    windows.sort_by_key(|window| window.minutes);
    if windows.is_empty() {
        return None;
    }
    let credits = value["credits"].as_object().map(|credit| Credits {
        has: credit
            .get("has_credits")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        unlimited: credit
            .get("unlimited")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        balance: credit
            .get("balance")
            .and_then(|v| v.as_f64().or_else(|| v.as_str()?.trim().parse().ok()))
            .filter(|balance| balance.is_finite())
            .unwrap_or(0.0),
    });
    Some(CodexSnapshot {
        ts: now,
        windows,
        plan: value["plan_type"].as_str().map(str::to_owned),
        reached: rate["limit_reached"].as_bool().unwrap_or(false),
        email: session.email.clone(),
        workspace_id: Some(session.account_id.clone()),
        credits,
        refills: refills(&value["rate_limit_reset_credits"]),
    })
}

pub(super) fn read(
    account: &crate::accounts::Account,
) -> Result<Option<CodexSnapshot>, UsageError> {
    let dir = account.path();
    let Some(session) = usable_session(&dir, crate::time::now()) else {
        return Ok(None);
    };
    let response = get_json(
        &CLIENT,
        concat!("https://chatgpt.com/backend-api", "/wham/usage"),
        &session,
    );
    if matches!(&response, Err(RequestError::AuthRejected)) {
        REJECTED.lock().unwrap_or_else(|e| e.into_inner()).reject(dir, &session);
    }
    let Some(mut usage) = usage_snapshot(response, &session, crate::time::now())? else {
        return Ok(None);
    };
    // The usage response reports the count. Fetch expiry details only if credits exist.
    if usage
        .refills
        .as_ref()
        .is_some_and(|refills| refills.available > 0 && refills.expires_at.is_none())
    {
        let retry_key = format!("{}:refills", account.usage_key());
        let details = usage_retry::run(&retry_key, || get_json(
            &CLIENT,
            "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits",
            &session,
        ).map_err(|error| match error {
            RequestError::RateLimited(delay) => UsageError::rate_limited(delay),
            _ => UsageError::Failed("Codex refill details unavailable".into()),
        }));
        if let Some(details) = details.ok().as_ref().and_then(refills) {
            usage.refills = Some(details);
        }
    }
    Ok(Some(usage))
}

fn usage_snapshot(
    response: Result<Value, RequestError>,
    session: &Session,
    now: f64,
) -> Result<Option<CodexSnapshot>, UsageError> {
    match response {
        Ok(value) => snapshot(&value, session, now).map(Some).ok_or_else(|| {
            UsageError::Failed(crate::tr!("Codex 사용량 정보 없음", "No Codex usage info").into())
        }),
        Err(RequestError::RateLimited(delay)) => Err(UsageError::rate_limited(delay)),
        Err(RequestError::AuthRejected) => Ok(None),
        Err(RequestError::Server) => Err(UsageError::Failed(
            crate::tr!(
                "Codex 사용량 서버 응답 없음",
                "The Codex usage server is temporarily unavailable"
            )
            .into(),
        )),
        Err(RequestError::Unavailable) => Err(UsageError::Failed(
            crate::tr!(
                "Codex 사용량 조회 실패",
                "Could not fetch Codex usage"
            )
            .into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    fn jwt(value: Value) -> String {
        let payload = serde_json::to_vec(&value).unwrap();
        let encoded = crate::b64::encode(&payload)
            .replace('+', "-")
            .replace('/', "_");
        format!("header.{}.signature", encoded.trim_end_matches('='))
    }

    #[test]
    fn uses_current_chatgpt_file_sessions_only() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("auth.json");
        let mut auth = json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "access_token": jwt(json!({"exp": 500})),
                "refresh_token": "unused-fixture",
                "id_token": jwt(json!({"email":"tracker@example.test"})),
                "account_id": "workspace-test"
            }
        });
        let fixture = serde_json::to_vec(&auth).unwrap();
        std::fs::write(&file, &fixture).unwrap();
        let session = load_session(dir.path(), 100.0).unwrap();
        assert_eq!(session.account_id, "workspace-test");
        assert_eq!(session.email.as_deref(), Some("tracker@example.test"));
        assert_eq!(std::fs::read(&file).unwrap(), fixture);
        assert!(load_session(dir.path(), 445.0).is_none());

        let config = dir.path().join("config.toml");
        for text in [
            r#"cli_auth_credentials_store = "keyring""#,
            r#"chatgpt_base_url = "https://test.invalid""#,
        ] {
            std::fs::write(&config, text).unwrap();
            assert!(load_session(dir.path(), 100.0).is_none());
        }
        std::fs::write(&config, r#"cli_auth_credentials_store = "file""#).unwrap();
        auth["auth_mode"] = json!("apikey");
        std::fs::write(&file, serde_json::to_vec(&auth).unwrap()).unwrap();
        assert!(load_session(dir.path(), 100.0).is_none());
    }

    #[test]
    fn only_authentication_rejection_selects_the_cli_after_an_http_response() {
        let session = Session {
            expires_at: 4000.0,
            access_token: "test-token".into(),
            account_id: "workspace-test".into(),
            email: None,
        };
        assert!(
            usage_snapshot(Err(RequestError::AuthRejected), &session, 100.0)
                .unwrap()
                .is_none()
        );
        for error in [RequestError::Unavailable, RequestError::Server] {
            assert!(matches!(
                usage_snapshot(Err(error), &session, 100.0),
                Err(UsageError::Failed(_))
            ));
        }
        assert!(matches!(
            usage_snapshot(Ok(json!({"unexpected": "shape"})), &session, 100.0),
            Err(UsageError::Failed(_))
        ));
        let before = crate::time::now();
        let result = usage_snapshot(
            Err(RequestError::RateLimited(Some(Duration::from_secs(120)))),
            &session,
            100.0,
        );
        assert!(matches!(result, Err(UsageError::RateLimited(at)) if at >= before + 120.0));
    }

    #[test]
    fn rejected_codex_token_waits_after_failed_cli_recovery_and_new_token_bypasses_wait() {
        let dir = tempfile::tempdir().unwrap();
        let now = crate::time::now();
        let write = |serial: &str| {
            let data = json!({
                "auth_mode": "chatgpt",
                "tokens": { "account_id": "workspace-test", "access_token": jwt(json!({"exp": now + 1000.0, "serial": serial})) }
            });
            std::fs::write(dir.path().join("auth.json"), data.to_string()).unwrap();
        };
        write("old");
        let session = usable_session(dir.path(), now).unwrap();
        REJECTED.lock().unwrap().reject(dir.path().to_path_buf(), &session);
        assert!(usable_session(dir.path(), now).is_none());
        assert_eq!(login_status(dir.path()).unwrap().logged_in, Some(false));
        assert!(recover_with_cli::<()>(dir.path(), || Err("fixture failure".into())).is_err());
        assert!(recover_with_cli::<()>(dir.path(), || panic!("recovery must wait")).is_err());
        write("new");
        assert!(usable_session(dir.path(), now).is_some());
        assert_eq!(login_status(dir.path()).unwrap().logged_in, Some(true));
        assert_eq!(recover_with_cli(dir.path(), || Ok(17)), Ok(17));
    }

    #[test]
    fn reads_a_single_response_and_preserves_workspace_scoped_usage() {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        let session = Session {
            expires_at: 4000.0,
            access_token: "test-token".into(),
            account_id: "workspace-test".into(),
            email: None,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/wham/usage", listener.local_addr().unwrap());
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
            assert!(request.starts_with("get /wham/usage http/1.1"));
            assert!(request.contains("\r\nauthorization: bearer test-token\r\n"));
            assert!(request.contains("\r\nchatgpt-account-id: workspace-test\r\n"));
            let response = json!({
                "account_id":"workspace-test", "plan_type":"pro", "credits":{"has_credits":true,"balance":"12.5"},
                "rate_limit": {"limit_reached":false,"primary_window":{
                    "limit_window_seconds":604800,"reset_at":1000,"used_percent":37.5}, "secondary_window":null},
                "rate_limit_reset_credits":{"available_count":0}
            }).to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        });
        let client = ureq::Agent::config_builder()
            .proxy(None)
            .timeout_global(Some(Duration::from_secs(3)))
            .build()
            .new_agent();
        let mut value = get_json(&client, &url, &session).unwrap();
        server.join().unwrap();
        let usage = snapshot(&value, &session, 100.0).unwrap();
        assert_eq!(usage.windows.len(), 1);
        assert_eq!(usage.windows[0].minutes, 10080);
        assert_eq!(usage.windows[0].used_percent, 37.5);
        assert_eq!(usage.credits.unwrap().balance, 12.5);
        assert_eq!(usage.refills.unwrap().available, 0);
        value["account_id"] = json!("other-workspace");
        assert!(snapshot(&value, &session, 100.0).is_none());
    }
}
