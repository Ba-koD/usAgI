//! 설정 파일(config.toml)과 상태 기록(state.json: 알림 기록, 화면 설정).
//!
//! Windows: %APPDATA%\usAgI   macOS: ~/Library/Application Support/usAgI   기타: ~/.config/usAgI

use serde::Deserialize;
use std::path::PathBuf;

pub const TEMPLATE: &str = r#"# usAgI 고급 설정 — 대부분은 설정 창에서 변경 가능.
# 저장 후 설정 창의 '새로고침'을 누르면 바로 반영.

[alerts]
# 알림 영역 아이콘 모습이 바뀌는 사용량(%). 알림 기준은 설정 창에서 그래프마다 추가 (기본 없음).
tired_percent = 80
notify = true

[data]
# Claude Code 로그 폴더를 더 읽을 때 (사용 중인지 표시용). 보통 빈 값.
claude_dirs = []
# 표시할 공급자: "claude", "codex"
providers = ["claude", "codex"]
"#;

/// 영어 화면일 때 새로 만드는 설정 파일 (키는 같음)
pub const TEMPLATE_EN: &str = r#"# usAgI advanced settings. Most options are in the settings window.
# After saving, press Refresh in the settings window to apply.

[alerts]
# Usage (%) at which the tray icon changes. Alert thresholds are added per graph in the settings window (none by default).
tired_percent = 80
notify = true

[data]
# Extra Claude Code log folders to read (for the in-use indicator). Usually empty.
claude_dirs = []
# Providers to show: "claude", "codex"
providers = ["claude", "codex"]
"#;

#[derive(Clone, Debug)]
pub struct Config {
    pub tired_percent: f64,
    pub notify: bool,
    pub claude_dirs: Vec<String>,
    pub poll_seconds: f64,
    pub providers: Vec<String>,
    pub error: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tired_percent: 80.0,
            notify: true,
            claude_dirs: Vec::new(),
            poll_seconds: 3.0,
            providers: vec!["claude".into(), "codex".into()],
            error: None,
        }
    }
}

#[derive(Deserialize, Default)]
struct FileAlerts {
    tired_percent: Option<f64>,
    notify: Option<bool>,
}

#[derive(Deserialize, Default)]
struct FileData {
    claude_dirs: Option<Vec<String>>,
    poll_seconds: Option<f64>,
    providers: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
struct FileConfig {
    #[serde(default)]
    alerts: FileAlerts,
    #[serde(default)]
    data: FileData,
}

/// 포터블 모드: exe 옆에 `data` 폴더가 있으면 설정·데이터를 모두 거기에 둠 (VS Code와 같은 방식).
/// 설치 없이 폴더째 옮겨 쓰는 용도로, %APPDATA%와 시작 메뉴에는 아무것도 남기지 않음.
/// (예전 표시 파일 `usagi.portable`도 계속 인식)
pub fn portable_dir() -> Option<PathBuf> {
    static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        if cfg!(test) {
            return None;
        }
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?;
        #[cfg(target_os = "macos")]
        if let Some(data) = mac_portable_dir(dir) {
            return Some(data);
        }
        let data = dir.join("data");
        is_portable_layout(dir).then_some(data)
    })
    .clone()
}

/// exe 폴더가 포터블 배치인지: `data\usagi.portable` 표시 파일, 예전 표시 파일(exe 옆 `usagi.portable`),
/// 또는 이미 usAgI 데이터(`config.toml`·`state.json`)가 든 `data` 폴더. 이름만 `data`인 다른 폴더는 쓰지 않음
fn is_portable_layout(dir: &std::path::Path) -> bool {
    dir.join("usagi.portable").exists() || contains_portable_data(&dir.join("data"))
}

fn contains_portable_data(data: &std::path::Path) -> bool {
    data.is_dir() && ["usagi.portable", "config.toml", "state.json"].iter().any(|file| data.join(file).is_file())
}

/// Keep portable data outside the replaceable bundle. Older updaters may
/// already have moved the only copy into .app.old when this version starts.
#[cfg(any(target_os = "macos", test))]
fn mac_portable_dir(executable_dir: &std::path::Path) -> Option<PathBuf> {
    let contents = executable_dir.parent()?;
    let bundle = contents.parent()?;
    if executable_dir.file_name()? != "MacOS" || contents.file_name()? != "Contents" || bundle.extension()? != "app" {
        return None;
    }
    let destination = bundle.with_extension("data");
    if contains_portable_data(&destination) {
        return Some(destination);
    }

    let previous_dir = bundle.with_extension("app.old").join("Contents/MacOS");
    let source = if is_portable_layout(executable_dir) {
        executable_dir.join("data")
    } else if is_portable_layout(&previous_dir) {
        previous_dir.join("data")
    } else {
        return None;
    };
    if destination.symlink_metadata().is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) {
        let ready = std::fs::create_dir_all(&source)
            .and_then(|()| std::fs::OpenOptions::new().create(true).append(true).open(source.join("usagi.portable")).map(drop));
        if ready.is_ok() && std::fs::rename(&source, &destination).is_ok() {
            return Some(destination);
        }
        if !source.exists() && contains_portable_data(&destination) {
            return Some(destination);
        }
    }
    // Keep using the original directory if the bundle's parent is read-only
    // or the destination is occupied by another folder.
    Some(source)
}

pub fn is_portable() -> bool {
    portable_dir().is_some()
}

pub fn config_dir() -> PathBuf {
    // 테스트는 절대 실제 사용자 데이터(state.json 등)를 건드리지 않음
    if cfg!(test) {
        return std::env::temp_dir().join(format!("usagi-test-{}", std::process::id()));
    }
    if let Some(dir) = portable_dir() {
        return dir;
    }
    let home = crate::logs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or(home).join("usAgI")
    } else if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support").join("usAgI")
    } else {
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config")).join("usAgI")
    }
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn ensure_file() -> std::io::Result<PathBuf> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir)?;
    let path = config_path();
    if !path.exists() {
        std::fs::write(&path, crate::tr!(TEMPLATE, TEMPLATE_EN))?;
    }
    Ok(path)
}

pub fn load() -> Config {
    let mut cfg = Config::default();
    let text = match ensure_file().and_then(std::fs::read_to_string) {
        Ok(t) => t,
        Err(e) => {
            cfg.error = Some(crate::tr!(format!("설정 파일 읽기 실패: {e}"), format!("Failed to read the config file: {e}")));
            return cfg;
        }
    };
    let file: FileConfig = match toml::from_str(&text) {
        Ok(f) => f,
        Err(e) => {
            cfg.error = Some(crate::tr!(
                format!("설정 파일 형식 오류: {}", e.message()),
                format!("Config file format error: {}", e.message())
            ));
            return cfg;
        }
    };
    cfg.tired_percent = file.alerts.tired_percent.unwrap_or(80.0).clamp(1.0, 99.0);
    cfg.notify = file.alerts.notify.unwrap_or(true);
    cfg.claude_dirs = file.data.claude_dirs.unwrap_or_default().into_iter().filter(|d| !d.trim().is_empty()).collect();
    cfg.poll_seconds = file.data.poll_seconds.unwrap_or(3.0).clamp(1.0, 60.0);
    if let Some(p) = file.data.providers {
        cfg.providers = p.into_iter().map(|s| s.to_lowercase()).collect();
    }
    cfg
}

/// 설정 창에서 바꾼 값(state.json의 prefs)을 설정에 덮어씀
pub fn apply_prefs(cfg: &mut Config, state: &State) {
    cfg.notify = state.pref_bool("notify", cfg.notify);
    if let Some(seconds) = state.pref_f64("poll") {
        cfg.poll_seconds = seconds.clamp(1.0, 60.0);
    }
    if let Some(t) = state.pref_f64("tired") {
        cfg.tired_percent = t.clamp(50.0, 95.0);
    }
}

// ------------------------------------------------------------------ state.json

/// 알림 기록과 화면 설정. 작은 JSON 객체 하나.
#[derive(Clone, Debug, Default)]
pub struct State(pub serde_json::Map<String, serde_json::Value>);

impl State {
    pub fn path() -> PathBuf {
        #[cfg(not(test))]
        return config_dir().join("state.json");
        #[cfg(test)]
        {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            thread_local! { static ID: usize = NEXT.fetch_add(1, Ordering::Relaxed); }
            ID.with(|id| config_dir().join(format!("state-test-{id}.json")))
        }
    }

    pub fn load() -> Self {
        let path = Self::path();
        let Ok(text) = std::fs::read_to_string(&path) else { return Self::default() };
        match serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| v.as_object().cloned()) {
            Some(map) => State(map),
            None => {
                // 읽을 수 없는 파일: 빈 설정으로 덮어쓰기 전에 보관해 둠 (설정이 사라지지 않게)
                if !text.trim().is_empty() {
                    let _ = std::fs::copy(&path, path.with_extension(format!("json.bad-{}", crate::time::now() as i64)));
                }
                Self::default()
            }
        }
    }

    pub fn save(&self) {
        let _ = self.try_save();
    }

    pub(crate) fn try_save(&self) -> std::io::Result<()> {
        self.save_to(&Self::path())
    }

    fn save_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        write_json(path, &self.0)
    }
}

pub(crate) fn write_json(path: &std::path::Path, value: &impl serde::Serialize) -> std::io::Result<()> {
    let dir = path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer(tmp.as_file_mut(), value)?;
    // A short-lived file lock on Windows can block replacement.
    let mut retries = [8, 25].into_iter();
    loop {
        let failure = match tmp.persist(path) {
            Ok(_) => return Ok(()),
            Err(failure) => failure,
        };
        let Some(delay) = retries.next().filter(|_| {
            cfg!(windows) && matches!(failure.error.raw_os_error(), Some(5 | 32 | 33))
        }) else {
            return Err(failure.error);
        };
        tmp = failure.file;
        std::thread::sleep(std::time::Duration::from_millis(delay));
    }
}

impl State {
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.as_str())
    }

    pub fn set_str(&mut self, key: &str, value: &str) {
        self.0.insert(key.into(), serde_json::Value::String(value.into()));
    }

    pub fn pref_bool(&self, key: &str, default: bool) -> bool {
        self.0.get("prefs").and_then(|p| p.get(key)).and_then(|v| v.as_bool()).unwrap_or(default)
    }

    pub fn pref_f64(&self, key: &str) -> Option<f64> {
        self.0.get("prefs").and_then(|p| p.get(key)).and_then(|v| v.as_f64())
    }

    pub fn pref_str(&self, key: &str, default: &str) -> String {
        self.0
            .get("prefs")
            .and_then(|p| p.get(key))
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    }

    /// 저장된 언어 설정(prefs.locale)을 지금 언어로
    pub fn apply_locale(&self) {
        crate::i18n::set(crate::i18n::resolve(&self.pref_str("locale", "system")));
    }

    pub fn set_pref(&mut self, key: &str, value: serde_json::Value) {
        let prefs = self.0.entry("prefs").or_insert_with(|| serde_json::json!({}));
        if !prefs.is_object() {
            *prefs = serde_json::json!({});
        }
        prefs[key] = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_portable_data_survives_replacement_by_an_older_updater() {
        let root = tempfile::tempdir().unwrap();
        let executable_dir = root.path().join("usAgI.app/Contents/MacOS");
        let previous = root.path().join("usAgI.app.old/Contents/MacOS/data");
        let profile = previous.join("profiles/codex-1");
        std::fs::create_dir_all(&executable_dir).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(previous.join("state.json"), b"{\"fixture\":true}").unwrap();
        std::fs::write(profile.join("fixture.txt"), b"existing profile").unwrap();

        let destination = root.path().join("usAgI.data");
        assert_eq!(mac_portable_dir(&executable_dir), Some(destination.clone()));
        assert_eq!(std::fs::read(destination.join("state.json")).unwrap(), b"{\"fixture\":true}");
        assert_eq!(std::fs::read(destination.join("profiles/codex-1/fixture.txt")).unwrap(), b"existing profile");
        assert_eq!(mac_portable_dir(&executable_dir), Some(destination));
    }

    #[test]
    fn mac_portable_data_stays_available_if_the_destination_is_occupied() {
        let root = tempfile::tempdir().unwrap();
        let executable_dir = root.path().join("usAgI.app/Contents/MacOS");
        let source = executable_dir.join("data");
        let destination = root.path().join("usAgI.data");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(source.join("state.json"), b"state").unwrap();
        std::fs::write(destination.join("unrelated"), b"keep").unwrap();

        assert_eq!(mac_portable_dir(&executable_dir), Some(source.clone()));
        assert_eq!(std::fs::read(source.join("state.json")).unwrap(), b"state");
        assert_eq!(std::fs::read(destination.join("unrelated")).unwrap(), b"keep");
    }

    #[test]
    fn both_templates_have_the_same_settings() {
        for text in [TEMPLATE, TEMPLATE_EN] {
            let f: FileConfig = toml::from_str(text).unwrap();
            assert_eq!(f.alerts.tired_percent, Some(80.0));
            assert_eq!(f.alerts.notify, Some(true));
            assert_eq!(f.data.claude_dirs, Some(Vec::new()));
            assert_eq!(f.data.providers, Some(vec!["claude".to_string(), "codex".to_string()]));
        }
    }
}
