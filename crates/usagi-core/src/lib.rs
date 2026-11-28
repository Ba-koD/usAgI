//! usAgI 공통 코어 — Windows / macOS / 모바일이 같이 쓰는 로직.
//!
//! - [`logs`]: Claude Code 로컬 로그 증분 리더 (활동 감지, 한도 메시지)
//! - [`tracker`]: 토끼 상태 (활동, 한도 메시지, 정확한 값 적용, 정확한 값 기반 낮잠 예보)
//! - [`texts`], [`i18n`]: 화면 문구(한국어·영어)와 시간 표기
//! - [`animator`]: 상태 + 시간 → 포즈
//! - [`rabbit`]: 단색 실루엣 토끼 렌더러 (알파 마스크)
//! - [`accounts`]: 공급자별 계정(로컬 프로필 폴더)과 Claude Code 연결
//! - [`bridge`]: Claude Code 상태 표시줄로 받은 정확한 사용량 (로그인·토큰 없음)
//! - [`usage_cmd`]: Claude 사용량 직접 조회와 Claude Code `/usage` 연동
//! - [`probe`]: `/usage`가 안 될 때 공식 CLI로 아주 작은 요청을 보내 사용률 받기
//! - [`codex`]: Codex 로그의 정확한 사용률
//! - [`login`]: 계정 로그인 상태 확인과 공식 로그인 실행
//! - [`logos`]: 공급자 로고
//! - [`config`], [`engine`]: 설정 파일, 백그라운드 폴링과 알림 판단
//! - [`controller`], [`ui`]: 설정 화면 상태와 모든 명령 처리 (Windows·macOS·Linux가 같은 화면·같은 동작)
//! - [`update`], [`version`]: 자동 업데이트 공통 부분 (확인·받기·서명 검증)
//! - `net`: HTTPS 공통 (TLS, OS 프록시·PAC를 따르는 연결)

pub mod accounts;
pub mod alert_rules;
pub mod animator;
pub mod b64;
pub mod bridge;
pub mod changelog;
mod claude_oauth;
pub mod codex;
pub mod config;
mod connection;
pub mod controller;
pub mod engine;
pub mod fallback;
pub mod i18n;
pub mod location;
pub mod login;
mod log_watch;
pub mod logos;
pub mod logs;
mod net;
pub mod private_dir;
pub mod probe;
pub mod rabbit;
mod desktop;
pub mod sha256;
pub mod settings_window;
pub mod texts;
pub mod time;
pub mod tracker;
pub mod ui;
pub mod update;
pub mod usage_cmd;
mod usage_http;
mod usage_retry;
pub mod version;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const CODENAME: &str = "Lop";
