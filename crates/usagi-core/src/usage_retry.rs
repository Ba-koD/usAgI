//! Backoff for scheduled and manual usage checks, shared by both providers.

use crate::{config, time, usage_http};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

const DEFAULT_DELAY: Duration = Duration::from_secs(5 * 60);
const MAX_DELAY: Duration = Duration::from_secs(60 * 60);
const RETRY_FILE: &str = "usage-retries.json";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum UsageMode {
    WithCli,
    ApiOnly,
}

#[derive(Debug, PartialEq)]
pub(crate) enum UsageError {
    Failed(String),
    RateLimited(f64),
    Deferred,
    Busy,
    NoApiSession,
    ProfileChanged,
    IdentityChanged,
}

impl UsageError {
    pub fn rate_limited(delay: Option<Duration>) -> Self {
        Self::RateLimited(retry_at(delay))
    }

    pub fn attempted(&self) -> bool {
        !matches!(self, Self::Deferred | Self::Busy | Self::NoApiSession)
    }

    pub fn auth_recheck(&self) -> bool {
        matches!(self, Self::Failed(message) if crate::fallback::auth_error(message))
    }
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Failed(message) => message,
            Self::RateLimited(_) | Self::Deferred => crate::tr!(
                "사용량 조회 제한",
                "Usage checks are rate limited"
            ),
            Self::Busy => crate::tr!("사용량 확인 중", "Usage check in progress"),
            Self::NoApiSession => crate::tr!("사용량 API 로그인 없음", "No usage API session"),
            Self::ProfileChanged => crate::tr!("Claude 대체 프로필 인증 변경 · 다시 확인 중", "Claude backup profile sign-in changed · Rechecking"),
            Self::IdentityChanged => crate::tr!("프로필 계정 변경 · 다시 연결 필요", "Profile account changed · Reconnect required"),
        })
    }
}

#[derive(Default)]
struct RetryState {
    deadlines: HashMap<String, f64>,
    in_flight: HashSet<String>,
}

impl RetryState {
    fn load(dir: &Path, now: f64) -> Self {
        let mut deadlines: HashMap<String, f64> = usage_http::read_bounded(&dir.join(RETRY_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        deadlines.retain(|_, at| at.is_finite() && *at > now);
        let max_at = now + MAX_DELAY.as_secs_f64();
        let mut adjusted = false;
        for at in deadlines.values_mut().filter(|at| **at > max_at) {
            *at = max_at;
            adjusted = true;
        }
        let state = Self {
            deadlines,
            in_flight: HashSet::new(),
        };
        if adjusted {
            state.persist(dir);
        }
        state
    }

    fn persist(&self, dir: &Path) {
        use std::io::Write as _;
        let _ = (|| {
            std::fs::create_dir_all(dir)?;
            let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
            serde_json::to_writer(tmp.as_file_mut(), &self.deadlines)?;
            tmp.flush()?;
            tmp.persist(dir.join(RETRY_FILE))?;
            Ok::<(), std::io::Error>(())
        })();
    }
}

static STATE: LazyLock<Mutex<RetryState>> =
    LazyLock::new(|| Mutex::new(RetryState::load(&config::config_dir(), time::now())));

fn retry_at(delay: Option<Duration>) -> f64 {
    let delay = delay
        .filter(|delay| !delay.is_zero())
        .unwrap_or(DEFAULT_DELAY)
        .min(MAX_DELAY);
    time::now() + delay.as_secs_f64()
}

/// Keep an elapsed deadline until the account actually resumes its checks.
pub(crate) fn deadline(account_id: &str) -> Option<f64> {
    STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .deadlines
        .get(account_id)
        .copied()
}

pub(crate) fn is_running(account_id: &str) -> bool {
    STATE.lock().unwrap_or_else(|e| e.into_inner()).in_flight.contains(account_id)
}

pub(crate) fn forget(account_id: &str) {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    state.in_flight.remove(account_id);
    let details = format!("{account_id}:refills");
    state.in_flight.remove(&details);
    let changed = state.deadlines.remove(account_id).is_some();
    if state.deadlines.remove(&details).is_some() || changed {
        state.persist(&config::config_dir());
    }
}

pub(crate) fn run<T>(
    account_id: &str,
    attempt: impl FnOnce() -> Result<T, UsageError>,
) -> Result<T, UsageError> {
    {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if state
            .deadlines
            .get(account_id)
            .is_some_and(|at| *at > time::now())
        {
            return Err(UsageError::Deferred);
        }
        if !state.in_flight.insert(account_id.to_owned()) {
            return Err(UsageError::Busy);
        }
    }
    let result = attempt();
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if !state.in_flight.remove(account_id) {
        return result;
    }
    let now = time::now();
    let changed = match &result {
        Err(UsageError::RateLimited(at)) => {
            state.deadlines.insert(account_id.to_owned(), *at);
            true
        }
        _ if state.deadlines.get(account_id).is_some_and(|at| *at <= now) => {
            state.deadlines.remove(account_id);
            true
        }
        _ => false,
    };
    if changed {
        state.persist(&config::config_dir());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_deadlines_and_ignores_elapsed_entries_on_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut state = RetryState::default();
        state.deadlines.insert("claude-1".into(), 200.0);
        state.in_flight.insert("claude-1".into());
        state.persist(dir.path());
        state.deadlines.insert("codex-1".into(), 300.0);
        state.persist(dir.path());

        let restarted = RetryState::load(dir.path(), 220.0);
        assert_eq!(
            restarted.deadlines,
            HashMap::from([("codex-1".into(), 300.0)])
        );
        assert!(restarted.in_flight.is_empty());
    }

    #[test]
    fn caps_new_and_stored_retry_after_to_one_hour() {
        let now = time::now();
        let at = retry_at(Some(Duration::from_secs(86_400)));
        assert!((3600.0..3602.0).contains(&(at - now)));
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(RETRY_FILE), r#"{"codex-1":86400,"claude-1":1300}"#).unwrap();
        let state = RetryState::load(dir.path(), 1000.0);
        assert_eq!(state.deadlines.get("codex-1"), Some(&4600.0));
        assert_eq!(state.deadlines.get("claude-1"), Some(&1300.0));
        assert_eq!(RetryState::load(dir.path(), 1001.0).deadlines, state.deadlines);
    }

    #[test]
    fn one_account_waits_without_blocking_another() {
        let id = "retry-state-test";
        let scheduled = time::now() + 120.0;
        assert_eq!(
            run::<()>(id, || Err(UsageError::RateLimited(scheduled))),
            Err(UsageError::RateLimited(scheduled))
        );
        assert_eq!(
            run::<()>(id, || panic!("deferred request must not run")),
            Err(UsageError::Deferred)
        );
        assert_eq!(run("unaffected-account", || Ok(17)), Ok(17));

        {
            let mut state = STATE.lock().unwrap();
            state.deadlines.insert(id.into(), time::now() - 1.0);
        }
        assert_eq!(run(id, || Ok(42)), Ok(42));
        assert_eq!(deadline(id), None);
    }
}
