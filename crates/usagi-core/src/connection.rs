//! 계정 연결 준비 한 번 (백그라운드). CLI가 없으면 공식 설치 프로그램으로 설치한 뒤 이어서 진행.
//! 컨트롤러가 수명을 관리함.
use crate::{accounts, install, login};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

pub(crate) enum Destination {
    /// 기본 프로필이 이미 구독 계정으로 로그인됨 → 로그인 없이 바로 추가
    Existing { profile: PathBuf, program: PathBuf },
    /// 공식 로그인 진행
    SignIn(PathBuf),
}

pub(crate) struct Connection {
    pub provider: &'static str,
    pub account_id: Option<String>,
    result: mpsc::Receiver<Result<Destination, String>>,
    cancel: Arc<AtomicBool>,
}

impl Connection {
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
        if install::installing(self.provider) {
            let cli = install::cli_name(self.provider);
            crate::tr!(
                format!("{cli} 미설치 · 공식 설치 프로그램으로 설치 중"),
                format!("{cli} not installed · installing with the official installer")
            )
        } else {
            crate::tr!("계정 연결 준비 중", "Preparing the account connection").into()
        }
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
    // 사용자가 직접 누른 연결이라 최근 설치 실패와 상관없이 다시 시도
    let programs = install::ensure(provider, true)?;
    for program in programs {
        if cancel.load(Ordering::Relaxed) {
            return Err(crate::tr!("연결 취소", "Connection canceled").into());
        }
        if login::usable_program(provider, &program) {
            return Ok(
                match profile.filter(|_| login::has_subscription(provider, &program)) {
                    Some(profile) => Destination::Existing { profile, program },
                    None => Destination::SignIn(program),
                },
            );
        }
    }
    let (cli, hint) = (install::cli_name(provider), login::install_hint(provider));
    Err(crate::tr!(format!("{cli} 실행 실패. {hint}"), format!("Failed to run {cli}. {hint}")))
}
