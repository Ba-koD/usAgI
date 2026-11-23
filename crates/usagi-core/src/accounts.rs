//! 계정 = 공급자별 로컬 프로필 폴더. 로그인 정보는 저장하지 않습니다.
//!
//! - Claude: Claude Code 설정 폴더 (기본 ~/.claude, CLAUDE_CONFIG_DIR로 다른 프로필 가능)
//!   연결 가능한 계정은 그 폴더의 settings.json에 usAgI 상태 표시줄 브리지를 넣습니다.
//!   원래 쓰던 상태 표시줄이 있으면 지우지 않고 이어서 실행(chain)하고, 연결 해제 시 되돌립니다.
//! - Codex: Codex 홈 폴더 (기본 ~/.codex). 로그에 정확한 사용률이 있어 연결 과정이 필요 없습니다.

use crate::config::State;
use crate::logs::home_dir;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::sync::atomic::{AtomicU64, Ordering};

static CHANGES: Mutex<()> = Mutex::new(());

pub(crate) fn lock_changes() -> MutexGuard<'static, ()> {
    CHANGES.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn new_revision() -> u64 {
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = (crate::time::now() * 1_000_000.0) as u64;
    let previous = LAST.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| Some(now.max(n + 1)));
    now.max(previous.unwrap() + 1)
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatuslineBackup {
    Missing,
    Present(serde_json::Value),
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
    #[serde(default)]
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statusline_original: Option<StatuslineBackup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) identity: Option<crate::login::Identity>,
}

impl Account {
    pub fn path(&self) -> PathBuf {
        let path = PathBuf::from(&self.dir);
        if !path.is_absolute() && path.parent() == Some(Path::new("profiles"))
            && path.file_name().is_some_and(|name| name == self.id.as_str())
        {
            return profiles_dir().join(&self.id);
        }
        path
    }
    pub fn is_claude(&self) -> bool {
        self.provider == "claude"
    }

    pub fn usage_key(&self) -> String {
        if self.revision == 0 { self.id.clone() } else { format!("{}-r{}", self.id, self.revision) }
    }

    pub(crate) fn commit_usage(&self, write: impl FnOnce() -> std::io::Result<()>) -> Result<(), crate::usage_retry::UsageError> {
        let _guard = lock_changes();
        if load(&State::load()).iter().any(|current| {
            current.id == self.id && current.revision == self.revision
                && current.provider == self.provider && current.dir == self.dir
        }) {
            write().map_err(|error| crate::usage_retry::UsageError::Failed(crate::tr!(
                format!("사용량 저장 실패 ({error})"),
                format!("Could not save usage ({error})")
            )))?;
        }
        Ok(())
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
    entries(state).filter_map(parse_account).collect()
}

fn entries(state: &State) -> impl Iterator<Item = &serde_json::Value> {
    state.0.get("accounts").and_then(|v| v.as_array()).into_iter().flatten()
}

fn parse_account(value: &serde_json::Value) -> Option<Account> {
    let mut acc: Account = serde_json::from_value(value.clone()).ok()?;
    if acc.dir.is_empty() || !matches!(acc.provider.as_str(), "claude" | "codex") {
        return None;
    }
    let path = acc.path();
    // Older portable versions stored absolute paths. Relocate only a missing
    // managed path when the matching profile already exists beside this app.
    let destination = profiles_dir().join(&acc.id);
    let moved = crate::config::is_portable() && path.is_absolute() && !path.exists()
        && path.ends_with(Path::new("data").join("profiles").join(&acc.id))
        && validate_managed_profile(&destination).is_ok();
    if moved || is_managed_path(&path) {
        acc.dir = if moved { destination } else { path }.to_string_lossy().into();
    }
    Some(acc)
}

pub(crate) fn has_unread_accounts(state: &State) -> bool {
    state.0.get("accounts").is_some_and(|v| !v.is_array())
        || entries(state).any(|value| parse_account(value).is_none())
}

pub fn save(state: &mut State, accounts: &[Account]) -> std::io::Result<()> {
    if state.0.get("accounts").is_some_and(|v| !v.is_array()) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "accounts is not a list"));
    }
    let mut updated = state.clone();
    let mut saved = entries(state).filter(|value| parse_account(value).is_none()).cloned().collect::<Vec<_>>();
    for account in accounts {
        let mut account = account.clone();
        let path = account.path();
        if is_managed_path(&path) && path.file_name().is_some_and(|name| name == account.id.as_str()) {
            account.dir = format!("profiles/{}", account.id);
        }
        saved.push(serde_json::to_value(account)?);
    }
    updated.0.insert("accounts".into(), serde_json::Value::Array(saved));
    updated.try_save()?;
    *state = updated;
    Ok(())
}

/// usAgI가 계정마다 만드는 프로필 폴더 (CLAUDE_CONFIG_DIR / CODEX_HOME 로 씀)
pub fn profiles_dir() -> PathBuf {
    crate::config::config_dir().join("profiles")
}

/// usAgI가 만든 프로필인지 (지울 때 로그아웃 후 폴더도 정리).
/// 프로필 폴더 **바로 아래** 한 단계만 인정해요. 이 판정으로 폴더를 지우므로,
/// `..`가 섞였거나 이름만 비슷한 옆 폴더(`profiles2`)는 관리 대상이 아니에요.
pub fn is_managed(acc: &Account) -> bool {
    is_managed_path(&acc.path())
}

fn is_managed_path(p: &Path) -> bool {
    use std::path::Component;
    if p.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return false;
    }
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(name)) => !name.is_empty() && parent == profiles_dir(),
        _ => false,
    }
}

pub(crate) fn validate_managed_profile(path: &Path) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    let root = profiles_dir();
    if !is_managed_path(path) || !plain_directory(&root)? || !plain_directory(path)?
        || std::fs::canonicalize(path)?.parent() != Some(std::fs::canonicalize(root)?.as_path())
    {
        return Err(Error::from(ErrorKind::PermissionDenied));
    }
    Ok(())
}

pub(crate) fn delete_managed_profile(path: &Path) -> std::io::Result<()> {
    // Only remove the app-owned directory. Calling provider logout could also
    // clear a shared credential store.
    validate_managed_profile(path)?;
    std::fs::remove_dir_all(path)
}

fn plain_directory(path: &Path) -> std::io::Result<bool> {
    let meta = std::fs::symlink_metadata(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Ok(false);
        }
    }
    Ok(meta.is_dir())
}

const INACTIVE_MARKER: &str = ".usagi-inactive";

pub(crate) fn profile_is_marked(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(INACTIVE_MARKER)).is_ok()
}

pub(crate) fn fallback_profile_available(dir: &Path) -> bool {
    dir.is_dir() && !profile_is_marked(dir)
}

/// Fresh or discarded sign-ins may not supply credentials to other cards.
pub(crate) fn mark_profile_inactive(dir: &Path) -> std::io::Result<bool> {
    validate_managed_profile(dir)?;
    match std::fs::File::create_new(dir.join(INACTIVE_MARKER)) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

pub(crate) fn unmark_profile(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(INACTIVE_MARKER));
}

pub(crate) fn managed_profiles(root: &Path) -> Vec<PathBuf> {
    if !plain_directory(root).unwrap_or(false) {
        return Vec::new();
    }
    let Ok(entries) = std::fs::read_dir(root) else { return Vec::new() };
    let mut paths: Vec<_> = entries.filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            name.to_str().and_then(|name| name.split_once('-')).is_some_and(|(provider, suffix)| {
                matches!(provider, "claude" | "codex") && suffix.bytes().all(|b| b.is_ascii_digit())
                    && suffix.parse::<u64>().is_ok_and(|number| number > 0)
            })
        })
        .map(|entry| entry.path())
        .filter(|path| plain_directory(path).unwrap_or(false))
        .collect();
    paths.sort();
    paths
}

/// 이미 있는 프로필 폴더도 이 사용자 전용으로 (앱 시작 때 한 번)
pub fn protect_profiles() -> std::io::Result<()> {
    let base = profiles_dir();
    match plain_directory(&base) {
        Ok(true) if crate::private_dir::make_private(&base) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
        Ok(_) => Err(std::io::ErrorKind::PermissionDenied.into()),
    }
}

/// 새 계정용 빈 프로필을 만들고 계정 항목을 돌려줌 (이어서 공식 로그인을 띄움)
pub fn create_profile(provider: &str, existing: &[Account]) -> std::io::Result<Account> {
    let base = profiles_dir();
    std::fs::create_dir_all(&base)?;
    if !plain_directory(&base)? {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    // 공식 CLI의 로그인 파일이 들어갈 자리: 이 사용자만 접근 (상위 폴더 권한을 물려받지 않게)
    protect_profiles()?;
    let id = new_id(provider, existing);
    let dir = base.join(&id);
    std::fs::create_dir(&dir)?;
    if let Err(e) = mark_profile_inactive(&dir) {
        let _ = std::fs::remove_dir(&dir);
        return Err(e);
    }
    #[cfg(unix)]
    if !crate::private_dir::make_private(&dir) {
        let _ = std::fs::remove_file(dir.join(INACTIVE_MARKER));
        let _ = std::fs::remove_dir(&dir);
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    Ok(Account { id, provider: provider.into(), dir: dir.to_string_lossy().into(), label: String::new(), show: true, chain: None, revision: new_revision(), statusline_original: None, identity: None })
}

pub fn new_id(provider: &str, existing: &[Account]) -> String {
    (1..)
        .map(|n| format!("{provider}-{n}"))
        .find(|id| !existing.iter().any(|a| &a.id == id) && !profiles_dir().join(id).exists())
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

/// 이 위치의 실행 파일을 상태 표시줄 명령으로 걸어도 되는지.
/// Windows는 큰따옴표 안에서도 Git Bash가 `$(…)`·백틱·`$변수`를, cmd가 `%변수%`를 풀어 쓰므로 그런 글자가 든 경로는 걸지 않음
pub fn linkable(exe: &Path) -> bool {
    !cfg!(windows) || !exe.to_string_lossy().chars().any(|c| matches!(c, '$' | '`' | '%' | '"'))
}

pub fn is_our_command(cmd: &str) -> bool {
    cmd.contains("statusline --account")
}

fn statusline_command(map: &serde_json::Map<String, serde_json::Value>) -> Option<&str> {
    map.get("statusLine")?.get("command")?.as_str()
}

fn owns_statusline(cmd: &str, acc: &Account) -> bool {
    cmd.rsplit_once(" statusline --account ").is_some_and(|(exe, id)| {
        let same = id == acc.id || id.strip_prefix(acc.id.as_str()).and_then(|tail| tail.strip_prefix("-r"))
            .is_some_and(|revision| revision.parse::<u64>().is_ok());
        same && exe.starts_with(['"', '\'']) && exe.ends_with(['"', '\''])
    })
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
            .ok_or_else(|| std::io::Error::other(crate::tr!("settings.json 형식 읽기 실패", "settings.json could not be parsed"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
        Err(e) => Err(e),
    }
}

fn write_settings(path: &Path, map: &serde_json::Map<String, serde_json::Value>) -> std::io::Result<()> {
    use std::io::Write;
    let text = serde_json::to_string_pretty(map).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.usagi-tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    // 원래 파일의 권한을 그대로 (사용자가 0600으로 좁혀 둔 settings.json을 넓히지 않게). 만들 때부터 그 권한으로
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        if let Ok(meta) = std::fs::metadata(path) {
            options.mode(meta.permissions().mode() & 0o777);
        }
    }
    let mut file = options.open(&tmp)?;
    file.write_all((text + "\n").as_bytes())?;
    drop(file);
    std::fs::rename(&tmp, path)
}

pub fn link_state(acc: &Account) -> Link {
    if !acc.path().is_dir() {
        return Link::Missing(crate::tr!("폴더 없음", "Folder not found").into());
    }
    match read_settings(&settings_path(acc)) {
        Ok(map) => {
            if statusline_command(&map).is_some_and(|cmd| owns_statusline(cmd, acc)) {
                Link::Linked
            } else {
                Link::NotLinked
            }
        }
        Err(e) => Link::Missing(e.to_string()),
    }
}

/// 아직 연결하지 않았거나 기존 브리지의 실행 명령을 갱신해야 하는지.
pub fn needs_link(acc: &Account, exe: &Path) -> bool {
    if !acc.is_claude() || !acc.path().is_dir() || !linkable(exe) {
        return false;
    }
    match read_settings(&settings_path(acc)) {
        Ok(map) => match statusline_command(&map) {
            Some(cmd) if is_our_command(cmd) => owns_statusline(cmd, acc) && cmd != bridge_command(exe, &acc.usage_key()),
            _ => true,
        },
        Err(_) => false,
    }
}

/// Claude Code 프로필에 브리지를 넣습니다. 처음 한 번은 settings.json을 백업합니다.
pub fn link(acc: &mut Account, exe: &Path) -> std::io::Result<()> {
    if !linkable(exe) {
        return Err(std::io::Error::other(crate::tr!("실행 파일 경로에 $ ` % 글자가 있어 연결 불가", "Can't link: the app path contains $, ` or %")));
    }
    let path = settings_path(acc);
    let mut map = read_settings(&path)?;
    let backup = acc.path().join("settings.json.usagi-backup");
    if path.exists() && !backup.exists() {
        std::fs::copy(&path, &backup)?;
    }
    let original = (!statusline_command(&map).is_some_and(|cmd| owns_statusline(cmd, acc)))
        .then(|| {
            let value = map.get("statusLine").cloned().map_or(StatuslineBackup::Missing, StatuslineBackup::Present);
            (value, statusline_command(&map).map(str::to_owned))
        });
    if statusline_command(&map).is_some_and(|cmd| is_our_command(cmd) && !owns_statusline(cmd, acc)) {
        return Err(std::io::Error::other(crate::tr!("다른 usAgI 계정의 실시간 연결 사용 중", "Live link in use by another usAgI account")));
    }
    map.insert(
        "statusLine".into(),
        serde_json::json!({ "type": "command", "command": bridge_command(exe, &acc.usage_key()), "padding": 0 }),
    );
    write_settings(&path, &map)?;
    if let Some((original, command)) = original {
        acc.statusline_original = Some(original);
        acc.chain = command;
    }
    Ok(())
}

/// 연결 해제: 원래 상태 표시줄로 되돌리거나 항목을 지웁니다.
pub fn unlink(acc: &mut Account) -> std::io::Result<()> {
    unlink_with_backup(acc).map(|_| ())
}

pub(crate) struct UnlinkedStatusline {
    bridge: serde_json::Value,
    restored: Option<serde_json::Value>,
}

pub(crate) fn unlink_with_backup(acc: &mut Account) -> std::io::Result<Option<UnlinkedStatusline>> {
    let path = settings_path(acc);
    let mut map = read_settings(&path)?;
    let backup = if statusline_command(&map).is_some_and(|cmd| owns_statusline(cmd, acc)) {
        let bridge = map["statusLine"].clone();
        let restored = original_statusline(acc);
        if let Some(value) = &restored {
            map.insert("statusLine".into(), value.clone());
        } else {
            map.remove("statusLine");
        }
        write_settings(&path, &map)?;
        Some(UnlinkedStatusline { bridge, restored })
    } else {
        None
    };
    acc.chain = None;
    acc.statusline_original = None;
    Ok(backup)
}

impl UnlinkedStatusline {
    pub(crate) fn rollback(self, acc: &Account) -> std::io::Result<()> {
        let path = settings_path(acc);
        let mut map = read_settings(&path)?;
        if map.get("statusLine") != self.restored.as_ref() {
            return Err(std::io::Error::other(crate::tr!("상태 표시줄 설정이 별도로 변경됨", "Status line settings changed separately")));
        }
        map.insert("statusLine".into(), self.bridge);
        write_settings(&path, &map)
    }
}

fn original_statusline(acc: &Account) -> Option<serde_json::Value> {
    if let Some(original) = &acc.statusline_original {
        return match original {
            StatuslineBackup::Present(value) => Some(value.clone()),
            StatuslineBackup::Missing => None,
        };
    }
    let backup = read_settings(&acc.path().join("settings.json.usagi-backup")).ok();
    backup.and_then(|mut map| {
        (statusline_command(&map) == acc.chain.as_deref()).then(|| map.remove("statusLine")).flatten()
    }).or_else(|| acc.chain.as_ref().map(|command| serde_json::json!({ "type": "command", "command": command })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_and_legacy_entries_survive_a_valid_account_edit() {
        use serde_json::json;
        let invalid = json!({"id": "claude-2", "dir": 123});
        let legacy = json!({"id": "browser-1", "provider": "claude", "dir": ""});
        let mut state = State::default();
        state.0.insert("accounts".into(), json!([
            {"id": "claude-1", "provider": "claude", "dir": "fixture"},
            invalid, legacy
        ]));
        let mut list = load(&state);
        assert_eq!(list.len(), 1);
        list[0].label = "Edited".into();
        save(&mut state, &list).unwrap();
        let rows = state.0["accounts"].as_array().unwrap();
        assert!(rows.contains(&invalid));
        assert!(rows.contains(&legacy));
        assert_eq!(load(&state)[0].label, "Edited");
    }

    #[test]
    fn managed_means_direct_child_of_profiles_dir() {
        let acc = |dir: PathBuf| Account { id: "codex-2".into(), provider: "codex".into(), dir: dir.to_string_lossy().into(), label: String::new(), show: true, chain: None, revision: 0, statusline_original: None, identity: None };
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
    fn explicit_delete_only_removes_the_managed_profile() {
        let root = profiles_dir();
        std::fs::create_dir_all(&root).unwrap();
        let managed = tempfile::tempdir_in(&root).unwrap();
        let external = tempfile::tempdir().unwrap();
        std::fs::write(managed.path().join("history"), "test").unwrap();
        std::fs::write(external.path().join("history"), "keep").unwrap();
        assert!(delete_managed_profile(external.path()).is_err());
        assert!(external.path().join("history").exists());
        delete_managed_profile(managed.path()).unwrap();
        assert!(!managed.path().exists());
    }

    #[test]
    fn paths_with_shell_characters_are_not_linked_on_windows() {
        assert!(linkable(Path::new(if cfg!(windows) { r"C:\Program Files\usAgI\usagi.exe" } else { "/Applications/usAgI.app/Contents/MacOS/usAgI" })));
        for bad in [r"C:\Tools\$(calc)\usagi.exe", r"C:\Tools\`x`\usagi.exe", r"C:\Tools\%TEMP%\usagi.exe"] {
            assert_eq!(linkable(Path::new(bad)), !cfg!(windows), "{bad}");
        }
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
            revision: 0,
            statusline_original: None,
            identity: None,
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
        let other = Account { id: "claude-2".into(), ..acc.clone() };
        assert!(!needs_link(&other, exe), "다른 계정의 브리지는 건드리지 않음");
        let moved = Path::new(if cfg!(windows) { r"D:\Tools\usagi.exe" } else { "/Tools/usagi" });
        assert!(needs_link(&acc, moved));
        link(&mut acc, moved).unwrap();
        assert!(!needs_link(&acc, moved));
        assert_eq!(acc.chain.as_deref(), Some("~/.claude/mine.sh"));

        unlink(&mut acc).unwrap();
        let text = std::fs::read_to_string(dir.join("settings.json")).unwrap();
        assert!(text.contains("~/.claude/mine.sh"));
        assert_eq!(link_state(&acc), Link::NotLinked);
        assert!(needs_link(&acc, exe));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
