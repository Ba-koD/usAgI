//! 계정 연결 준비 한 번 (백그라운드). CLI가 필요하면 설치 안내를 반환합니다.
//! 컨트롤러가 수명을 관리함.
use crate::{accounts, login};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

pub(crate) enum Destination {
    /// 기본 프로필이 이미 구독 계정으로 로그인됨 → 로그인 없이 바로 추가
    Existing {
        profile: PathBuf,
        program: Option<PathBuf>,
    },
    /// 공식 로그인 진행
    SignIn(PathBuf),
    NeedsCli,
}

pub(crate) struct Connection {
    pub provider: &'static str,
    pub account_id: Option<String>,
    result: mpsc::Receiver<Result<Destination, String>>,
    cancel: Arc<AtomicBool>,
}

impl Connection {
    pub fn restore(provider: &'static str, profile: PathBuf) -> std::io::Result<Self> {
        let (tx, result) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        std::thread::Builder::new().name("usagi-restore".into()).spawn(move || {
            let account: accounts::Account = serde_json::from_value(serde_json::json!({
                "id": profile.file_name().and_then(|name| name.to_str()).unwrap_or(provider),
                "provider": provider,
                "dir": profile,
            })).expect("profile account");
            let result = if login::status(&account).logged_in == Some(true) {
                Ok(Destination::Existing { profile, program: None })
            } else {
                Err(crate::tr!("보관된 프로필의 유효한 로그인 없음", "No usable sign-in in the archived profile").into())
            };
            let _ = tx.send(result);
        })?;
        Ok(Self { provider, account_id: None, result, cancel })
    }

    pub fn start(
        provider: &'static str,
        account_id: Option<String>,
        existing: &[accounts::Account],
    ) -> std::io::Result<Self> {
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, result) = mpsc::channel();
        let worker_cancel = cancel.clone();
        let default_profile = if account_id.is_none() {
            match provider {
                "claude" => accounts::default_claude_dir(),
                _ => accounts::default_codex_dir(),
            }
            .filter(|dir| {
                dir.is_dir()
                    && !existing.iter().any(|a| {
                        a.provider == provider && login::same_dir(&a.path(), Some(dir.clone()))
                    })
            })
        } else {
            None
        };
        std::thread::Builder::new()
            .name("usagi-connect".into())
            .spawn(move || {
                let _ = tx.send(resolve(provider, default_profile, &worker_cancel));
            })?;
        Ok(Self {
            provider,
            account_id,
            result,
            cancel,
        })
    }

    pub fn next(&self) -> Option<Result<Destination, String>> {
        match self.result.try_recv() {
            Ok(event) => Some(event),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(crate::tr!(
                "계정 연결 준비 중 오류",
                "Error while preparing the account connection"
            )
            .into())),
        }
    }

    /// 화면에 보일 진행 상황
    pub fn message(&self) -> String {
        crate::tr!("계정 연결 준비 중", "Preparing the account connection").into()
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn resolve(
    provider: &str,
    profile: Option<PathBuf>,
    cancel: &AtomicBool,
) -> Result<Destination, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err(crate::tr!("연결 취소", "Connection canceled").into());
    }
    if let Some(profile) = profile.as_ref().filter(|dir| login::has_file_session(provider, dir)) {
        return Ok(Destination::Existing {
            profile: profile.clone(),
            program: None,
        });
    }
    let programs = login::installed_programs(provider);
    if programs.is_empty() {
        return Ok(Destination::NeedsCli);
    }
    for program in programs {
        if cancel.load(Ordering::Relaxed) {
            return Err(crate::tr!("연결 취소", "Connection canceled").into());
        }
        if login::usable_program(provider, &program) {
            return Ok(
                match profile.filter(|_| login::has_subscription(provider, &program)) {
                    Some(profile) => Destination::Existing {
                        profile,
                        program: Some(program),
                    },
                    None => Destination::SignIn(program),
                },
            );
        }
    }
    let (cli, hint) = (login::cli_name(provider), login::install_hint(provider));
    Err(crate::tr!(
        format!("{cli} 실행 실패. {hint}"),
        format!("Failed to run {cli}. {hint}")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connects_a_valid_existing_codex_file_before_checking_for_the_cli() {
        let profile = tempfile::tempdir().unwrap();
        let claims = format!(r#"{{"exp":{:.0}}}"#, crate::time::now() + 3600.0);
        let payload = crate::b64::encode(claims.as_bytes());
        let token = format!("header.{}.signature", payload.trim_end_matches('='));
        std::fs::write(
            profile.path().join("auth.json"),
            serde_json::json!({
                "auth_mode": "chatgpt",
                "tokens": { "access_token": token, "account_id": "workspace-fixture" }
            })
            .to_string(),
        )
        .unwrap();
        let result = resolve(
            "codex",
            Some(profile.path().to_owned()),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(
            matches!(result, Destination::Existing { profile: path, program: None } if path == profile.path())
        );
    }
}
