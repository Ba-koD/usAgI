//! macOS 자동 업데이트 (GitHub Releases).
//!
//! 1. 확인: `api.github.com/repos/<저장소>/releases/latest`의 태그를 지금 버전과 비교
//! 2. 받기: `usAgI-mac.zip`을 받아 릴리스 본문 또는 기존 첨부의 체크섬과 서명으로 검증 후 임시 폴더에 풀기
//!    (앱이 직접 쓴 파일이라 격리 표시가 없고, 적용할 때도 한 번 더 지움)
//! 3. 적용: 지금 `.app`을 `.app.old`로 비켜 두고 새 `.app`을 그 자리에 놓은 뒤, 잠깐 뒤에 새 앱을 열고 종료
//!
//! 통신(네트워크 설정의 프록시·PAC 포함)과 검증은 코어의 [`usagi_core::update`].
//! Homebrew로 설치했으면 brew가 버전을 관리하므로 스스로 바꾸지 않고 `brew upgrade`를 안내해요.

use std::path::{Path, PathBuf};
use std::process::Command;

pub use usagi_core::update::{Release, REPO};

const ASSET: &str = "usAgI-mac.zip";

/// Homebrew로 설치했을 때의 업데이트 명령 (스스로 바꾸지 않음, 안내 문장은 코어가 언어에 맞춰 만듦)
pub const BREW_HINT: &str = "brew upgrade --cask usagi";

/// Homebrew로 설치한 앱인지 (그러면 brew upgrade로)
pub fn installed_by_brew() -> bool {
    let Some(bundle) = current_app().and_then(|path| path.canonicalize().ok()) else { return false };
    ["/opt/homebrew/Caskroom/usagi", "/usr/local/Caskroom/usagi"]
        .iter().any(|path| bundle.starts_with(path) || brew_has_bundle(Path::new(path), &bundle))
}

fn brew_has_bundle(cask: &Path, bundle: &Path) -> bool {
    let default_target = Path::new("/Applications/usAgI.app");
    if bundle == default_target && cask.join(".metadata").join(usagi_core::VERSION).is_dir() {
        return true;
    }
    bundle.file_name().and_then(|name| {
        cask.join(usagi_core::VERSION).join(name).canonicalize().ok()
    }).is_some_and(|installed| installed == bundle)
}

/// 최신 릴리스 확인. 지금보다 새 버전이면 Some.
pub fn check() -> Result<Option<Release>, String> {
    usagi_core::update::check(ASSET)
}

/// 지금 실행 중인 .app 묶음
pub fn current_app() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bundle = exe.parent()?.parent()?.parent()?;
    bundle.extension().is_some_and(|e| e == "app").then(|| bundle.to_path_buf())
}

/// 받아서 검증하고 임시 폴더에 푼 새 .app 경로
pub fn download(rel: &Release) -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("usagi-update-{}", rel.version));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let zip = dir.join(ASSET);
    let mut file = std::fs::File::create(&zip).map_err(|error| error.to_string())?;
    usagi_core::update::download(rel, &mut file)?;
    drop(file);
    let file = std::fs::File::open(&zip).map_err(|error| error.to_string())?;
    rel.verify_asset(file, ASSET, usagi_core::update::fetch_text)?;
    let ok = Command::new("/usr/bin/ditto").args(["-x", "-k"]).arg(&zip).arg(&dir).status().map_err(|e| e.to_string())?;
    let app = dir.join("usAgI.app");
    if !ok.success() || !["usAgI", "usagi-settings"].iter().all(|name| app.join("Contents/MacOS").join(name).is_file()) {
        return Err(usagi_core::tr!("받은 압축 파일에 usAgI.app 없음", "usAgI.app not found in the downloaded archive").into());
    }
    let _ = std::fs::remove_file(zip);
    if let Some(native) = stage_native_app(&app) {
        let _ = std::fs::remove_dir_all(app);
        Ok(native)
    } else {
        Ok(app)
    }
}

fn stage_native_app(app: &Path) -> Option<PathBuf> {
    let arch = if cfg!(target_arch = "aarch64") { "arm64" } else { "x86_64" };
    let native = app.parent()?.join("native/usAgI.app");
    // Thin a copy of the checked archive contents. The original remains
    // available if a host tool can't preserve its Mach-O signatures.
    let copied = Command::new("/usr/bin/ditto").args(["--arch", arch])
        .arg(app).arg(&native).output().ok()?.status.success();
    if !copied {
        return None;
    }
    Command::new("/usr/bin/codesign").args(["--verify", "--deep", "--strict"])
        .arg(&native).output().ok()?.status.success().then_some(native)
}

/// 새 .app으로 바꾸고 다시 시작 (성공하면 호출한 쪽이 종료해야 함)
pub fn apply(staged: &Path) -> Result<(), String> {
    let app = current_app().ok_or(usagi_core::tr!("앱 묶음(.app) 밖에서 실행 중이라 교체 불가", "Can't replace the app: not running from an .app bundle"))?;
    let old = app.with_extension("app.old");
    if usagi_core::config::portable_dir().is_some_and(|dir| dir.starts_with(&app) || dir.starts_with(&old)) {
        return Err(usagi_core::tr!(
            "포터블 데이터를 앱 밖으로 옮기지 못해 업데이트를 중단했습니다. 앱 폴더의 쓰기 권한이나 옆의 .data 폴더를 확인하세요.",
            "The update was stopped because portable data could not be moved outside the app. Check folder permissions or the .data folder next to the app."
        ).into());
    }
    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(&app, &old).map_err(|e| usagi_core::tr!(format!("업데이트 적용 실패 (폴더 권한 확인 필요): {e}"), format!("Couldn't apply the update (check folder permissions): {e}")))?;
    // 다른 디스크일 수 있어 rename 대신 ditto로 복사
    let ok = Command::new("/usr/bin/ditto").arg(staged).arg(&app).status().map(|s| s.success()).unwrap_or(false);
    if !ok {
        let _ = std::fs::remove_dir_all(&app);
        let _ = std::fs::rename(&old, &app);
        return Err(usagi_core::tr!("새 버전 복사 실패", "Couldn't copy the new version").into());
    }
    let _ = Command::new("/usr/bin/xattr").args(["-dr", "com.apple.quarantine"]).arg(&app).status();
    // Keep the previous app available after launch. Restore it if Launch Services rejects the replacement.
    let script = r#"while kill -0 "$3" 2>/dev/null; do sleep 0.2; done
if ! /usr/bin/open -n "$1"; then
    /bin/mv "$1" "$1.failed" && /bin/mv "$2" "$1" && /usr/bin/open -n "$1"
fi"#;
    match Command::new("/bin/sh").args(["-c", script, "usagi-update"])
        .arg(&app).arg(&old).arg(std::process::id().to_string()).spawn() {
        Ok(_) => Ok(()),
        Err(error) => {
            std::fs::remove_dir_all(&app).and_then(|()| std::fs::rename(&old, &app))
                .map_err(|rollback| format!("{error}: {rollback}"))?;
            Err(usagi_core::tr!(format!("다시 시작 실패: {error}"), format!("Couldn't restart: {error}")))
        }
    }
}
