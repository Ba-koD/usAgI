//! 로그인 폴더 폴백.
//!
//! 계정마다 정한 폴더(주 폴더)를 먼저 씀. 그 폴더의 로그인이 정말 끊겼을 때만 같은 계정으로
//! 로그인된 다른 폴더(usAgI 전용 프로필 → 기본 폴더)를 대신 쓰고, 주 폴더가 다시 로그인되면
//! 다음 확인 때 주 폴더로 돌아감.
//!
//! - "정말 끊김": 상태 명령이 만료라고 해도 바로 믿지 않고, 주 폴더로 사용량을 한 번 실제로 조회해 봄
//!   (CLI가 스스로 로그인을 갱신할 수 있으면 살아남음).
//! - "같은 계정": Claude는 이메일과 조직, Codex는 이메일과 ChatGPT 워크스페이스 ID.
//!   계정 정보를 모르면(직접 로그아웃 등) 폴백하지 않음.
//! - 다른 계정 카드가 주 폴더로 쓰는 폴더는 후보에서 뺌 (같은 값이 두 카드에 뜨지 않게).
//! - 유효한 파일 로그인은 로컬에서 확인하고, 그 외에는 공식 CLI 상태 명령을 사용.

use crate::accounts::{self, Account};
use crate::login::{self, LoginStatus};
use crate::usage_retry::{self, UsageError, UsageMode};
use std::path::{Path, PathBuf};

/// 대신 쓸 수 있는 폴더: 이 계정 이름의 전용 프로필 → 같은 공급자의 다른 전용 프로필 → 기본 폴더.
/// 주 폴더, 다른 계정의 주 폴더, 없는 폴더는 뺌.
pub fn candidates(acc: &Account, all: &[Account]) -> Vec<PathBuf> {
    candidates_in(&accounts::profiles_dir(), acc, all)
}

fn candidates_in(profiles_dir: &Path, acc: &Account, all: &[Account]) -> Vec<PathBuf> {
    let prefix = format!("{}-", acc.provider);
    let mut profiles: Vec<_> = accounts::managed_profiles(profiles_dir).into_iter()
        .filter(|path| path.file_name().is_some_and(|name| name.to_string_lossy().starts_with(&prefix)))
        .collect();
    // 이 계정 이름(claude-1)의 프로필이 맨 앞
    profiles.sort_by_key(|p| p.file_name().is_none_or(|n| n.to_string_lossy() != acc.id.as_str()));
    let default = if acc.is_claude() { accounts::default_claude_dir() } else { accounts::default_codex_dir() };
    let mut out: Vec<PathBuf> = Vec::new();
    for dir in profiles.into_iter().chain(default) {
        let taken = login::same_dir(&dir, Some(acc.path()))
            || all.iter().any(|a| a.id != acc.id && a.provider == acc.provider && login::same_dir(&dir, Some(a.path())))
            || out.iter().any(|d| login::same_dir(d, Some(dir.clone())));
        if !taken && accounts::fallback_profile_available(&dir) {
            out.push(dir);
        }
    }
    out
}

/// 같은 계정인지 (대소문자·앞뒤 공백 무시). 기준 이메일을 모르면 false.
/// 조직은 둘 다 알 때만 비교 (Claude 개인·팀 조직이 다른 같은 이메일 구분)
pub fn same_account(want_email: Option<&str>, want_org: Option<&str>, email: Option<&str>, org: Option<&str>) -> bool {
    let eq = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
    match (want_email.map(str::trim).filter(|s| !s.is_empty()), email) {
        (Some(a), Some(b)) if eq(a, b) => match (want_org, org) {
            (Some(x), Some(y)) => eq(x, y),
            _ => true,
        },
        _ => false,
    }
}

/// 같은 계정을 다른 폴더로 쓰는 사본 (id는 그대로라 사용량·알림은 같은 카드로)
pub fn at(acc: &Account, dir: &Path) -> Account {
    Account { dir: dir.to_string_lossy().into(), ..acc.clone() }
}

pub(crate) fn same_claude_identity(expected: &LoginStatus, actual: &LoginStatus) -> bool {
    let matches = |left: Option<&str>, right: Option<&str>| same_account(left, None, right, None);
    matches(expected.email.as_deref(), actual.email.as_deref()) && match (&expected.organization_id, &actual.organization_id) {
        (None, None) => matches(expected.org.as_deref(), actual.org.as_deref()),
        (expected, actual) => matches(expected.as_deref(), actual.as_deref()),
    }
}

pub(crate) fn claude_usage(primary: &Account, target: &Account, approved: Option<&LoginStatus>, mode: UsageMode) -> Result<crate::bridge::BridgeFile, UsageError> {
    let status = || match mode {
        UsageMode::WithCli => Some(login::status(target)),
        UsageMode::ApiOnly => login::file_status(&target.provider, &target.path()),
    };
    if primary.dir == target.dir {
        if primary.identity.is_none() {
            return crate::usage_cmd::read_usage_for_session(target, None, mode);
        }
        let current = status().ok_or(UsageError::NoApiSession)?;
        verify_primary(primary, login::Identity::from_status(&current))?;
        let fingerprint = current.credential_fingerprint;
        let usage = crate::usage_cmd::read_usage_for_session(target, fingerprint, mode)?;
        verify_primary(primary, status().as_ref().and_then(login::Identity::from_status))?;
        return Ok(usage);
    }
    let approved = approved.filter(|status| status.logged_in == Some(true) && status.fallback.as_deref() == Some(target.dir.as_str()))
        .ok_or(UsageError::ProfileChanged)?;
    checked_claude_usage(
        || {
            if !accounts::fallback_profile_available(&target.path()) {
                return false;
            }
            status().is_some_and(|status| {
                status.logged_in == Some(true) && same_claude_identity(approved, &status)
                    && approved.credential_fingerprint == status.credential_fingerprint
            })
        },
        || crate::usage_cmd::read_usage_for_session(target, approved.credential_fingerprint, mode),
    )
}

fn checked_claude_usage(
    mut verify: impl FnMut() -> bool,
    read: impl FnOnce() -> Result<crate::bridge::BridgeFile, UsageError>,
) -> Result<crate::bridge::BridgeFile, UsageError> {
    if !verify() {
        return Err(UsageError::ProfileChanged);
    }
    let usage = read()?;
    if !verify() {
        return Err(UsageError::ProfileChanged);
    }
    Ok(usage)
}

fn same_codex_workspace(email: &str, workspace: &str, actual_email: Option<&str>, actual_workspace: Option<&str>) -> bool {
    !workspace.is_empty() && actual_workspace == Some(workspace) && same_account(Some(email), None, actual_email, None)
}

fn codex_identity(acc: &Account) -> Option<(String, String)> {
    crate::codex::profile_identity(&acc.path()).or_else(|| {
        let cached = crate::codex::load(&acc.usage_key())?;
        Some((cached.workspace_id?, cached.email?))
    })
}

pub(crate) fn codex_usage(primary: &Account, target: &Account, mode: UsageMode) -> Result<crate::codex::CodexSnapshot, UsageError> {
    if primary.dir == target.dir {
        verify_primary(primary, login::file_status(&target.provider, &target.path()).as_ref().and_then(login::Identity::from_status))?;
        let usage = crate::codex::refresh_usage(target, mode)?;
        verify_primary(primary, login::Identity::from_codex(&usage))?;
        return Ok(usage);
    }
    let mismatch = || UsageError::Failed(crate::tr!("Codex 워크스페이스 인증 불일치", "Codex workspace authentication mismatch").into());
    let (workspace, email) = codex_identity(primary).ok_or_else(mismatch)?;
    let (candidate_workspace, candidate_email) = crate::codex::profile_identity(&target.path()).ok_or_else(mismatch)?;
    if !same_codex_workspace(&email, &workspace, Some(&candidate_email), Some(&candidate_workspace)) {
        return Err(mismatch());
    }
    let usage = crate::codex::refresh_usage(target, mode)?;
    if !accounts::fallback_profile_available(&target.path())
        || !same_codex_workspace(&email, &workspace, usage.email.as_deref(), usage.workspace_id.as_deref()) {
        return Err(mismatch());
    }
    Ok(usage)
}

fn verify_primary(acc: &Account, actual: Option<login::Identity>) -> Result<(), UsageError> {
    if acc.identity.as_ref().zip(actual.as_ref()).is_some_and(|(expected, actual)| expected.conflicts(actual)) {
        return Err(UsageError::IdentityChanged);
    }
    Ok(())
}

/// 로그인 상태, 대신 쓰는 폴더, 사용량 확인이 겹치거나 제한됐을 때 재확인 시각.
pub fn resolve(acc: &Account, all: &[Account]) -> (LoginStatus, Option<PathBuf>, Option<f64>) {
    let mut st = login::status(acc);
    if let Err(error) = verify_primary(acc, login::Identity::from_status(&st)) {
        st.logged_in = Some(false);
        st.error = Some(error.to_string());
        return (st, None, None);
    }
    if st.logged_in != Some(false) {
        return (st, None, None);
    }
    let pending = |st: LoginStatus, at| (LoginStatus { logged_in: None, ..st }, None, Some(at));
    // 1) 주 폴더를 한 번 실제로 써 봄
    if let Some(result) = revive(acc) {
        match result {
            Ok(()) => return (LoginStatus { logged_in: Some(true), ..login::status(acc) }, None, None),
            Err(e) => if let Some(at) = retry_recovery(acc, &e) { return pending(st, at); },
        }
    }
    // 2) 같은 계정으로 로그인된 다른 폴더
    if !acc.is_claude() && codex_identity(acc).is_none() {
        return (st, None, None);
    }
    for dir in candidates(acc, all) {
        let cand = at(acc, &dir);
        let mut shown = if acc.is_claude() {
            let cs = login::status(&cand);
            if cs.logged_in != Some(true)
                || !accounts::fallback_profile_available(&dir)
                || !same_claude_identity(&st, &cs) {
                continue;
            }
            cs
        } else {
            match codex_usage(acc, &cand, UsageMode::WithCli) {
                Ok(s) => {
                    let shown = LoginStatus {
                        logged_in: Some(true),
                        email: s.email.clone(),
                        org: Some("ChatGPT".into()),
                        checked_at: s.ts,
                        ..Default::default()
                    };
                    if let Err(error) = acc.commit_usage(|| crate::codex::save(&acc.usage_key(), &s)) {
                        return (LoginStatus { error: Some(error.to_string()), ..st }, None, None);
                    }
                    shown
                }
                Err(e) => {
                    if let Some(at) = retry_recovery(acc, &e) {
                        return pending(st, at);
                    }
                    continue;
                }
            }
        };
        shown.fallback = Some(dir.to_string_lossy().into());
        return (shown, Some(dir), None);
    }
    (st, None, None)
}

/// 주 폴더로 사용량을 한 번 조회 (성공하면 그 값도 저장)
fn revive(acc: &Account) -> Option<Result<(), UsageError>> {
    let key = acc.usage_key();
    Some(match acc.provider.as_str() {
        "claude" if crate::usage_cmd::available(acc) => {
            claude_usage(acc, acc, None, UsageMode::WithCli).and_then(|f| acc.commit_usage(|| crate::bridge::save(&key, f)))
        }
        "codex" => crate::codex::recover_usage(acc).and_then(|s| {
            verify_primary(acc, login::Identity::from_codex(&s))?;
            acc.commit_usage(|| crate::codex::save(&key, &s))
        }),
        _ => return None,
    })
}

fn retry_recovery(acc: &Account, error: &UsageError) -> Option<f64> {
    let soon = crate::time::now() + 1.0;
    match error {
        UsageError::Busy => Some(soon),
        UsageError::Deferred => Some(usage_retry::deadline(&acc.usage_key()).unwrap_or(soon).max(soon)),
        UsageError::RateLimited(at) => Some(at.max(soon)),
        UsageError::Failed(_) | UsageError::NoApiSession | UsageError::ProfileChanged | UsageError::IdentityChanged => None,
    }
}

/// 사용량 조회 오류가 로그인 문제로 보이는지 (그럼 로그인 상태를 바로 다시 확인)
pub fn auth_error(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    ["authenticat", "not logged in", "log in again", "login expired", "session expired", "unauthorized", "401", "403", "인증 불일치"]
        .iter()
        .any(|k| m.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_account_needs_known_matching_email_and_org() {
        assert!(same_account(Some("A@b.c"), Some("Org"), Some("a@b.c "), Some("org")));
        assert!(same_account(Some("a@b.c"), None, Some("a@b.c"), Some("org")), "조직을 모르면 이메일만");
        assert!(!same_account(Some("a@b.c"), Some("Org"), Some("a@b.c"), Some("Other")), "같은 이메일, 다른 조직");
        assert!(!same_account(Some("a@b.c"), None, Some("x@b.c"), None));
        assert!(!same_account(None, None, Some("a@b.c"), None), "기준을 모르면 폴백 안 함");
        assert!(!same_account(Some(" "), None, Some(" "), None));
    }

    #[test]
    fn claude_matches_organization_identity_even_for_equal_labels() {
        let expected = LoginStatus {
            email: Some("claude@example.test".into()),
            org: Some("Team".into()),
            organization_id: Some("team-one".into()),
            ..Default::default()
        };
        let mut actual = expected.clone();
        assert!(same_claude_identity(&expected, &actual));
        actual.organization_id = Some("team-two".into());
        assert!(!same_claude_identity(&expected, &actual));
        actual.organization_id = None;
        assert!(!same_claude_identity(&expected, &actual));
    }

    #[test]
    fn claude_discards_usage_when_the_fallback_sign_in_changes_during_the_request() {
        assert!(matches!(checked_claude_usage(|| false, || panic!("unverified profile must not be queried")), Err(UsageError::ProfileChanged)));
        let mut identity = [true, false].into_iter();
        assert!(matches!(
            checked_claude_usage(|| identity.next().unwrap(), || Ok(crate::bridge::BridgeFile::default())),
            Err(UsageError::ProfileChanged)
        ));
        assert!(checked_claude_usage(|| true, || Ok(crate::bridge::BridgeFile::default())).is_ok());
    }

    #[test]
    fn codex_requires_the_same_workspace_even_for_the_same_email() {
        assert!(same_codex_workspace("a@b.c", "workspace-one", Some("A@b.c"), Some("workspace-one")));
        assert!(!same_codex_workspace("a@b.c", "workspace-one", Some("a@b.c"), Some("workspace-two")));
        assert!(!same_codex_workspace("a@b.c", "workspace-one", Some("a@b.c"), None));
        assert!(!same_codex_workspace("a@b.c", "workspace-one", Some("other@b.c"), Some("workspace-one")));
    }

    #[test]
    fn busy_recovery_has_a_short_recheck_instead_of_a_login_failure() {
        let account: Account = serde_json::from_value(serde_json::json!({
            "id": "codex-1", "provider": "codex", "dir": "profile"
        })).unwrap();
        let now = crate::time::now();
        let retry = retry_recovery(&account, &UsageError::Busy).unwrap();
        assert!((1.0..3.0).contains(&(retry - now)));
        assert_eq!(retry_recovery(&account, &UsageError::RateLimited(now + 300.0)), Some(now + 300.0));
    }

    #[test]
    fn candidates_prefer_own_profile_and_skip_taken_folders() {
        // 이 테스트만 쓰는 임시 profiles 폴더 (실제 사용자 폴더·다른 테스트와 분리)
        let base = std::env::temp_dir().join(format!("usagi-fallback-profiles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for name in ["claude-1", "claude-2", "claude-3", "codex-1"] {
            std::fs::create_dir_all(base.join(name)).unwrap();
        }
        let acc = |id: &str, dir: PathBuf| Account { id: id.into(), provider: "claude".into(), dir: dir.to_string_lossy().into(), label: String::new(), show: true, chain: None, revision: 0, statusline_original: None, identity: None };
        let elsewhere = std::env::temp_dir().join(format!("usagi-fallback-primary-{}", std::process::id()));
        std::fs::create_dir_all(&elsewhere).unwrap();
        let me = acc("claude-2", elsewhere.clone());
        // claude-3 폴더는 다른 카드가 주 폴더로 씀
        let other = acc("claude-9", base.join("claude-3"));
        let all = vec![me.clone(), other];
        let c = candidates_in(&base, &me, &all);
        let names: Vec<String> = c.iter().filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
        assert_eq!(names.first().map(String::as_str), Some("claude-2"), "이 계정 이름의 프로필이 먼저: {names:?}");
        assert!(names.contains(&"claude-1".to_string()));
        assert!(!names.contains(&"claude-3".to_string()), "다른 카드의 주 폴더는 빼야 함");
        assert!(!names.contains(&"codex-1".to_string()), "다른 공급자 프로필은 빼야 함");
        assert!(!c.iter().any(|p| login::same_dir(p, Some(elsewhere.clone()))), "주 폴더는 후보가 아님");
        // 주 폴더가 전용 프로필이면 그 프로필은 후보에서 빠짐
        let me2 = acc("claude-1", base.join("claude-1"));
        assert!(!candidates_in(&base, &me2, &[me2.clone()]).iter().any(|p| p.ends_with("claude-1")));
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    #[test]
    fn auth_errors_are_recognised() {
        assert!(auth_error("Failed to authenticate: OAuth session expired and could not be refreshed"));
        assert!(auth_error("Not logged in. Run claude auth login"));
        assert!(!auth_error("timeout after 15s"));
        assert!(!auth_error("Claude CLI did not respond"));
    }
}
