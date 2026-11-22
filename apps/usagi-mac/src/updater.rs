//! macOS 자동 업데이트 (GitHub Releases).
//!
//! 1. 확인: `api.github.com/repos/<저장소>/releases/latest`의 태그를 지금 버전과 비교
//! 2. 받기: `usAgI-mac.zip`(+ `usAgI-mac.zip.sha256`)을 curl로 받아 SHA-256 검증 후 임시 폴더에 풀기
//!    (curl로 받은 파일엔 격리 표시가 붙지 않아 "확인되지 않은 개발자" 경고가 없음)
//! 3. 적용: 지금 `.app`을 `.app.old`로 비켜 두고 새 `.app`을 그 자리에 놓은 뒤, 잠깐 뒤에 새 앱을 열고 종료
//!
//! Homebrew로 설치했으면 brew가 버전을 관리하므로 스스로 바꾸지 않고 `brew upgrade`를 안내해요.

use std::path::{Path, PathBuf};
use std::process::Command;

/// 업데이트를 받을 저장소 ("owner/name"). 릴리스 빌드(GitHub Actions)는 자기 저장소를 넣어 빌드함.
pub const REPO: &str = match option_env!("USAGI_UPDATE_REPO") {
    Some(r) => r,
    None => "Ba-koD/usAgI",
};

pub use usagi_core::update::Release;

/// Homebrew로 설치했을 때 안내 (스스로 바꾸지 않음)
pub const BREW_HINT: &str = "brew upgrade --cask usagi 로 업데이트.";

fn curl(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("/usr/bin/curl")
        // https만 (리다이렉트도), TLS 1.2 이상
        .args(["-fsSL", "--proto", "=https", "--proto-redir", "=https", "--tlsv1.2", "--max-time", "120"])
        .args(["-A", &format!("usAgI/{} (macOS)", usagi_core::VERSION)])
        .args(args)
        .output()
        .map_err(|e| format!("curl 실행 실패: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(if err.contains("404") {
            "올라온 릴리스 없음".into()
        } else if err.contains("403") || err.contains("429") {
            "GitHub 요청 한도 초과. 잠시 뒤 다시 확인".into()
        } else {
            "업데이트 서버 연결 실패 (인터넷 연결 확인 필요)".into()
        });
    }
    Ok(out.stdout)
}

/// Homebrew로 설치한 앱인지 (그러면 brew upgrade로)
pub fn installed_by_brew() -> bool {
    ["/opt/homebrew/Caskroom/usagi", "/usr/local/Caskroom/usagi"].iter().any(|p| Path::new(p).exists())
}

/// 최신 릴리스 확인. 지금보다 새 버전이면 Some.
pub fn check() -> Result<Option<Release>, String> {
    let body = curl(&["-H", "Accept: application/vnd.github+json", &usagi_core::update::latest_api(REPO)])?;
    usagi_core::update::parse_latest(&body, usagi_core::VERSION, REPO, "usAgI-mac.zip")
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
    let zip = dir.join("usAgI-mac.zip");
    curl(&["-o", &zip.to_string_lossy(), &rel.asset_url])?;
    // 체크섬은 꼭 있어야 하고 일치해야 함
    let want = usagi_core::update::parse_sha256(&String::from_utf8_lossy(&curl(&[&rel.sha_url])?)).ok_or("체크섬 수신 실패")?;
    let got = usagi_core::sha256::hex(&std::fs::read(&zip).map_err(|e| e.to_string())?);
    if got != want {
        return Err("받은 파일 손상 (체크섬 불일치). 다시 시도 필요".into());
    }
    let ok = Command::new("/usr/bin/ditto").args(["-x", "-k"]).arg(&zip).arg(&dir).status().map_err(|e| e.to_string())?;
    let app = dir.join("usAgI.app");
    if !ok.success() || !app.join("Contents/MacOS/usAgI").exists() {
        return Err("받은 압축 파일에 usAgI.app 없음".into());
    }
    Ok(app)
}

/// 새 .app으로 바꾸고 다시 시작 (성공하면 호출한 쪽이 종료해야 함)
pub fn apply(staged: &Path) -> Result<(), String> {
    let app = current_app().ok_or("앱 묶음(.app) 밖에서 실행 중이라 교체 불가")?;
    let old = app.with_extension("app.old");
    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(&app, &old).map_err(|e| format!("업데이트 적용 실패 (폴더 권한 확인 필요): {e}"))?;
    // 다른 디스크일 수 있어 rename 대신 ditto로 복사
    let ok = Command::new("/usr/bin/ditto").arg(staged).arg(&app).status().map(|s| s.success()).unwrap_or(false);
    if !ok {
        let _ = std::fs::remove_dir_all(&app);
        let _ = std::fs::rename(&old, &app);
        return Err("새 버전 복사 실패".into());
    }
    let _ = Command::new("/usr/bin/xattr").args(["-dr", "com.apple.quarantine"]).arg(&app).status();
    // 지금 프로세스가 끝난 뒤 새 앱을 열고 예전 앱을 지움
    let q = |p: &Path| format!("'{}'", p.to_string_lossy().replace('\'', "'\\''"));
    let script = format!("sleep 1; /usr/bin/open -n {}; sleep 3; rm -rf {}", q(&app), q(&old));
    Command::new("/bin/sh").args(["-c", &script]).spawn().map(|_| ()).map_err(|e| format!("다시 시작 실패: {e}"))
}
