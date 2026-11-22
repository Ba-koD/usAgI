//! 계정 = 공급자별 로컬 프로필 폴더. 로그인 정보는 저장하지 않습니다.
//!
//! - Claude: Claude Code 설정 폴더 (기본 ~/.claude, CLAUDE_CONFIG_DIR로 다른 프로필 가능)
//!   "연결"하면 그 폴더의 settings.json에 usAgI 상태 표시줄 브리지를 넣어 정확한 사용량을 받습니다.
//!   원래 쓰던 상태 표시줄이 있으면 지우지 않고 이어서 실행(chain)하고, 연결 해제 시 되돌립니다.
//! - Codex: Codex 홈 폴더 (기본 ~/.codex). 로그에 정확한 사용률이 있어 연결 과정이 필요 없습니다.

use crate::config::State;
use crate::logs::home_dir;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    /// "claude" | "codex"
    pub provider: String,
    pub dir: String,
    #[serde(default)]
    pub label: String,
    /// 작업 표시줄 위젯에 표시
    #[serde(default = "yes")]
    pub show: bool,
    /// 연결 전에 쓰던 상태 표시줄 명령 (이어서 실행)
    #[serde(default)]
    pub chain: Option<String>,
}

impl Account {
    pub fn path(&self) -> PathBuf {
        PathBuf::from(&self.dir)
    }
    pub fn is_claude(&self) -> bool {
        self.provider == "claude"
    }
}

pub fn default_claude_dir() -> Option<PathBuf> {
    if let Some(env) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        if let Some(first) = std::env::split_paths(&env).next() {
            return Some(first);
        }
    }
    home_dir().map(|h| h.join(".claude"))
}

pub fn default_codex_dir() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME").map(PathBuf::from).or_else(|| home_dir().map(|h| h.join(".codex")))
}

/// 등록한 계정. 처음엔 비어 있어요 (설정의 "+ 추가"로 공식 로그인해서 추가).
pub fn load(state: &State) -> Vec<Account> {
    state
        .0
        .get("accounts")
        .and_then(|v| serde_json::from_value::<Vec<Account>>(v.clone()).ok())
        .unwrap_or_default()
}

pub fn save(state: &mut State, accounts: &[Account]) {
    if let Ok(v) = serde_json::to_value(accounts) {
        state.0.insert("accounts".into(), v);
        state.save();
    }
}

/// usAgI가 계정마다 만드는 프로필 폴더 (CLAUDE_CONFIG_DIR / CODEX_HOME 로 씀)
pub fn profiles_dir() -> PathBuf {
    crate::config::config_dir().join("profiles")
}

/// usAgI가 만든 프로필인지 (지울 때 로그아웃 후 폴더도 정리).
/// 프로필 폴더 **바로 아래** 한 단계만 인정해요. 이 판정으로 폴더를 지우므로,
/// `..`가 섞였거나 이름만 비슷한 옆 폴더(`profiles2`)는 관리 대상이 아니에요.
pub fn is_managed(acc: &Account) -> bool {
    use std::path::Component;
    let p = acc.path();
    if p.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return false;
    }
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(name)) => !name.is_empty() && norm(parent) == norm(&profiles_dir()),
        _ => false,
    }
}

/// 새 계정용 빈 프로필을 만들고 계정 항목을 돌려줌 (이어서 공식 로그인을 띄움)
pub fn create_profile(provider: &str, existing: &[Account]) -> std::io::Result<Account> {
    let id = new_id(provider, existing);
    let dir = profiles_dir().join(&id);
    std::fs::create_dir_all(&dir)?;
    Ok(Account { id, provider: provider.into(), dir: dir.to_string_lossy().into(), label: String::new(), show: true, chain: None })
}

pub fn new_id(provider: &str, existing: &[Account]) -> String {
    (1..)
        .map(|n| format!("{provider}-{n}"))
        .find(|id| !existing.iter().any(|a| &a.id == id))
        .expect("id")
}

// ------------------------------------------------------------------ Claude Code 연결

/// 상태 표시줄 명령 (Claude Code가 셸로 실행). 셸이 해석할 글자가 끼어들지 않게 따옴표로 감싸요.
/// - Windows: bash/cmd 둘 다 읽을 수 있게 큰따옴표 + 슬래시 경로
/// - 그 외: 작은따옴표 (경로에 `$`, 백틱, 공백이 있어도 그대로)
pub fn bridge_command(exe: &Path, account_id: &str) -> String {
    let id: String = account_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    let exe = exe.to_string_lossy();
    if cfg!(windows) {
        format!("\"{}\" statusline --account {id}", exe.replace('\\', "/").replace('"', ""))
    } else {
        format!("'{}' statusline --account {id}", exe.replace('\'', "'\\''"))
    }
}

pub fn is_our_command(cmd: &str) -> bool {
    cmd.contains("statusline --account")
}

#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    /// usAgI 브리지가 들어가 있음
    Linked,
    /// 아직 연결 안 함
    NotLinked,
    /// 폴더나 settings.json이 없음/깨짐
    Missing(String),
}

fn settings_path(acc: &Account) -> PathBuf {
    acc.path().join("settings.json")
}

fn read_settings(path: &Path) -> std::io::Result<serde_json::Map<String, serde_json::Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Default::default()),
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .ok_or_else(|| std::io::Error::other("settings.json 형식 읽기 실패")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(e) => Err(e),
    }
}

fn write_settings(path: &Path, map: &serde_json::Map<String, serde_json::Value>) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(map).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.usagi-tmp");
    std::fs::write(&tmp, text + "\n")?;
    std::fs::rename(&tmp, path)
}

pub fn link_state(acc: &Account) -> Link {
    if !acc.path().is_dir() {
        return Link::Missing("폴더 없음".into());
    }
    match read_settings(&settings_path(acc)) {
        Ok(map) => {
            let cmd = map.get("statusLine").and_then(|s| s.get("command")).and_then(|c| c.as_str());
            if cmd.is_some_and(is_our_command) {
                Link::Linked
            } else {
                Link::NotLinked
            }
        }
        Err(e) => Link::Missing(e.to_string()),
    }
}

/// 실시간 연결(상태 표시줄 브리지)을 걸어야 하는지: 아직 없거나, 다른 위치의 usAgI(옮기기 전 경로)를 가리킬 때.
/// 폴더가 없거나 settings.json을 읽을 수 없으면 건드리지 않음.
pub fn needs_link(acc: &Account, exe: &Path) -> bool {
    if !acc.is_claude() || !acc.path().is_dir() {
        return false;
    }
    match read_settings(&settings_path(acc)) {
        Ok(map) => map.get("statusLine").and_then(|s| s.get("command")).and_then(|c| c.as_str()) != Some(bridge_command(exe, &acc.id).as_str()),
        Err(_) => false,
    }
}

/// Claude Code 프로필에 브리지를 넣습니다. 처음 한 번은 settings.json을 백업합니다.
pub fn link(acc: &mut Account, exe: &Path) -> std::io::Result<()> {
    let path = settings_path(acc);
    let mut map = read_settings(&path)?;
    let backup = acc.path().join("settings.json.usagi-backup");
    if path.exists() && !backup.exists() {
        std::fs::copy(&path, &backup)?;
    }
    if let Some(old) = map.get("statusLine").and_then(|s| s.get("command")).and_then(|c| c.as_str()) {
        if !is_our_command(old) {
            acc.chain = Some(old.to_string());
        }
    }
    map.insert(
        "statusLine".into(),
        serde_json::json!({ "type": "command", "command": bridge_command(exe, &acc.id), "padding": 0 }),
    );
    write_settings(&path, &map)
}

/// 연결 해제: 원래 상태 표시줄로 되돌리거나 항목을 지웁니다.
pub fn unlink(acc: &mut Account) -> std::io::Result<()> {
    let path = settings_path(acc);
    let mut map = read_settings(&path)?;
    let ours = map
        .get("statusLine")
        .and_then(|s| s.get("command"))
        .and_then(|c| c.as_str())
        .is_some_and(is_our_command);
    if ours {
        match acc.chain.take() {
            Some(old) => {
                map.insert("statusLine".into(), serde_json::json!({ "type": "command", "command": old }));
            }
            None => {
                map.remove("statusLine");
            }
        }
        write_settings(&path, &map)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_means_direct_child_of_profiles_dir() {
        let acc = |dir: PathBuf| Account { id: "codex-2".into(), provider: "codex".into(), dir: dir.to_string_lossy().into(), label: String::new(), show: true, chain: None };
        let base = profiles_dir();
        assert!(is_managed(&acc(base.join("codex-2"))));
        assert!(!is_managed(&acc(base.clone())), "프로필 폴더 자체");
        assert!(!is_managed(&acc(base.join("a").join("b"))), "두 단계 아래");
        assert!(!is_managed(&acc(base.join("..").join("..").join("Documents"))), ".. 로 빠져나감");
        assert!(!is_managed(&acc(base.join("codex-2").join("..").join("..").join("x"))));
        let sibling = PathBuf::from(format!("{}2", base.to_string_lossy())).join("x");
        assert!(!is_managed(&acc(sibling)), "이름만 비슷한 옆 폴더");
        assert!(!is_managed(&acc(PathBuf::from("relative"))));
    }

    #[test]
    fn bridge_command_is_quoted() {
        let cmd = bridge_command(Path::new(if cfg!(windows) { r"C:\Program Files\usAgI\usagi.exe" } else { "/Applications/us $(x)'AgI/usAgI" }), "claude-1; rm -rf ~");
        assert!(cmd.ends_with(" statusline --account claude-1rm-rf"), "{cmd}");
        assert!(is_our_command(&cmd));
        if cfg!(windows) {
            assert!(cmd.starts_with("\"C:/Program Files/usAgI/usagi.exe\" "));
        } else {
            assert!(cmd.starts_with("'/Applications/us $(x)'\\''AgI/usAgI' "), "{cmd}");
        }
    }

    #[test]
    fn link_and_unlink_preserve_other_settings_and_chain() {
        let dir = std::env::temp_dir().join(format!("usagi-acc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("settings.json"),
            r#"{"model":"opus","statusLine":{"type":"command","command":"~/.claude/mine.sh"},"zeta":1}"#,
        )
        .unwrap();
        let mut acc = Account {
            id: "claude-1".into(),
            provider: "claude".into(),
            dir: dir.to_string_lossy().into(),
            label: String::new(),
            show: true,
            chain: None,
        };
        assert_eq!(link_state(&acc), Link::NotLinked);
        link(&mut acc, Path::new(if cfg!(windows) { r"C:\Apps\usagi.exe" } else { "/Apps/usagi" })).unwrap();
        assert_eq!(link_state(&acc), Link::Linked);
        assert_eq!(acc.chain.as_deref(), Some("~/.claude/mine.sh"));
        let text = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        // 따옴표는 플랫폼마다 다름 (Windows 큰따옴표, 그 외 작은따옴표)
        let quoted = if cfg!(windows) { r#""C:/Apps/usagi.exe\" statusline --account claude-1"# } else { "'/Apps/usagi' statusline --account claude-1" };
        assert!(text.contains(quoted), "{text}");
        // 다른 설정과 순서 유지
        assert!(text.find("\"model\"").unwrap() < text.find("\"zeta\"").unwrap());
        assert!(dir.join("settings.json.usagi-backup").exists());

        // 이미 같은 명령이면 다시 걸 필요 없음, exe 위치가 바뀌면 다시 걸어야 함 (원래 상태 표시줄은 그대로 기억)
        let exe = Path::new(if cfg!(windows) { r"C:\Apps\usagi.exe" } else { "/Apps/usagi" });
        assert!(!needs_link(&acc, exe));
        let moved = Path::new(if cfg!(windows) { r"D:\Tools\usagi.exe" } else { "/Tools/usagi" });
        assert!(needs_link(&acc, moved));
        link(&mut acc, moved).unwrap();
        assert!(!needs_link(&acc, moved));
        assert_eq!(acc.chain.as_deref(), Some("~/.claude/mine.sh"));

        unlink(&mut acc).unwrap();
        let text = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(text.contains("~/.claude/mine.sh"));
        assert_eq!(link_state(&acc), Link::NotLinked);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
