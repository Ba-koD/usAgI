//! 자동 업데이트 공통 부분: GitHub 릴리스 정보 읽기와 받을 파일 고르기.
//! 내려받기·검증·바꿔 끼우기는 플랫폼마다 달라서 각 앱의 updater가 해요.

use crate::version;
use serde_json::Value;

/// 새 버전 정보
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// 표시용 버전 (v 없음)
    pub version: String,
    /// 릴리스 페이지 (github.com)
    pub page: String,
    pub notes: String,
    /// 이 플랫폼용 파일 (usagi.exe, usAgI-mac.zip …)
    pub asset_url: String,
    /// 같은 이름 + .sha256 (없는 릴리스는 받지 않음)
    pub sha_url: String,
}

/// 최신 릴리스 API 주소
pub fn latest_api(repo: &str) -> String {
    format!("https://api.github.com/repos/{repo}/releases/latest")
}

/// 릴리스 목록 페이지 (새 버전 페이지가 없을 때)
pub fn releases_page(repo: &str) -> String {
    format!("https://github.com/{repo}/releases")
}

/// 버전 글자로 쓸 수 있는 것만 (숫자·영문·`.`·`-`·`+`). 폴더 이름에 쓰이므로 `..`나 경로 글자는 안 됨.
fn safe_version(v: &str) -> bool {
    !v.is_empty() && v.len() <= 64 && !v.contains("..") && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+'))
}

/// `releases/latest` 응답을 읽음. 지금(`current`)보다 새 버전이고 `asset` 파일과 체크섬이 있으면 Some.
/// 받을 주소는 이 저장소(`repo`)의 릴리스 파일 주소여야 해요.
pub fn parse_latest(body: &[u8], current: &str, repo: &str, asset: &str) -> Result<Option<Release>, String> {
    let v: Value = serde_json::from_slice(body).map_err(|_| "릴리스 정보 읽기 실패".to_string())?;
    let tag = v.get("tag_name").and_then(|t| t.as_str()).unwrap_or("");
    if !version::is_newer(tag, current) {
        return Ok(None);
    }
    let version = version::clean(tag);
    if !safe_version(&version) {
        return Err("릴리스 버전 형식 오류".into());
    }
    let assets = v.get("assets").and_then(|a| a.as_array()).cloned().unwrap_or_default();
    let prefix = format!("https://github.com/{repo}/releases/download/").to_lowercase();
    let url_of = |name: &str| {
        assets
            .iter()
            .find(|a| a.get("name").and_then(|n| n.as_str()).is_some_and(|n| n.eq_ignore_ascii_case(name)))
            .and_then(|a| a.get("browser_download_url").and_then(|u| u.as_str()))
            .filter(|u| u.to_lowercase().starts_with(&prefix))
            .map(str::to_string)
    };
    let asset_url = url_of(asset).ok_or_else(|| format!("새 릴리스에 {asset} 파일 없음"))?;
    let sha_url = url_of(&format!("{asset}.sha256")).ok_or_else(|| format!("새 릴리스에 {asset}.sha256 (체크섬) 없음"))?;
    Ok(Some(Release {
        version,
        page: v.get("html_url").and_then(|u| u.as_str()).unwrap_or("").to_string(),
        notes: v.get("body").and_then(|b| b.as_str()).unwrap_or("").to_string(),
        asset_url,
        sha_url,
    }))
}

/// 체크섬 파일 내용("<64자리 hex>  파일이름")에서 해시만. 형식이 아니면 None.
pub fn parse_sha256(text: &str) -> Option<String> {
    let h = text.split_whitespace().next()?.to_lowercase();
    (h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())).then_some(h)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = r#"{"tag_name":"v0.2.0","html_url":"https://github.com/o/r/releases/tag/v0.2.0","body":"notes",
        "assets":[{"name":"usagi.exe","browser_download_url":"https://github.com/o/r/releases/download/v0.2.0/usagi.exe"},
                  {"name":"usagi.exe.sha256","browser_download_url":"https://github.com/o/r/releases/download/v0.2.0/usagi.exe.sha256"}]}"#;

    #[test]
    fn newer_release_with_asset() {
        let r = parse_latest(BODY.as_bytes(), "0.1.0", "o/r", "usagi.exe").unwrap().unwrap();
        assert_eq!(r.version, "0.2.0");
        assert!(r.asset_url.ends_with("/usagi.exe"));
        assert!(r.sha_url.ends_with(".sha256"));
    }

    #[test]
    fn same_version_is_none() {
        assert_eq!(parse_latest(BODY.as_bytes(), "0.2.0", "o/r", "usagi.exe").unwrap(), None);
    }

    #[test]
    fn missing_asset_is_error() {
        assert!(parse_latest(BODY.as_bytes(), "0.1.0", "o/r", "usAgI-mac.zip").is_err());
    }

    #[test]
    fn rejects_other_hosts_missing_checksum_and_odd_versions() {
        // 다른 저장소·다른 서버의 파일 주소는 받지 않음
        assert!(parse_latest(BODY.as_bytes(), "0.1.0", "someone/else", "usagi.exe").is_err());
        let evil = BODY.replace("https://github.com/o/r/releases/download/v0.2.0/usagi.exe\"", "https://evil.example/usagi.exe\"");
        assert!(parse_latest(evil.as_bytes(), "0.1.0", "o/r", "usagi.exe").is_err());
        // 체크섬 파일이 없으면 받지 않음
        let no_sha = BODY.replace("usagi.exe.sha256\",\"browser", "other.txt\",\"browser");
        assert!(parse_latest(no_sha.as_bytes(), "0.1.0", "o/r", "usagi.exe").is_err());
        // 버전이 폴더 이름으로 쓰이므로 경로 글자는 안 됨
        let odd = BODY.replace("\"tag_name\":\"v0.2.0\"", "\"tag_name\":\"v0.2.0-../../x\"");
        assert!(parse_latest(odd.as_bytes(), "0.1.0", "o/r", "usagi.exe").is_err());
        let pre = BODY.replace("\"tag_name\":\"v0.2.0\"", "\"tag_name\":\"v0.2.0-beta.1\"");
        assert_eq!(parse_latest(pre.as_bytes(), "0.1.0", "o/r", "usagi.exe").unwrap().unwrap().version, "0.2.0-beta.1");
    }

    #[test]
    fn checksum_file_format() {
        let h = "b4c4c0d84196b00ba360934efae3c67d683ebf23c01af88124fbaa742be4f864";
        assert_eq!(parse_sha256(&format!("{h}  usagi.exe\n")).as_deref(), Some(h));
        assert_eq!(parse_sha256(&h.to_uppercase()).as_deref(), Some(h));
        assert_eq!(parse_sha256("<html>Not Found</html>"), None);
        assert_eq!(parse_sha256(""), None);
    }
}
