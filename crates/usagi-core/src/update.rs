//! 자동 업데이트 공통 부분: GitHub 릴리스 정보 읽기, 받을 파일 고르기, 서명 검증.
//! 내려받기·바꿔 끼우기는 플랫폼마다 달라서 각 앱의 updater가 해요.
//!
//! 서명: 체크섬은 실행 파일과 같은 릴리스에서 오므로 릴리스를 바꿔치기한 쪽이 함께 바꿀 수 있어요.
//! 그래서 저장소에 없는 비밀 키(릴리스 워크플로의 비밀값)로 만든 minisign 서명(`<파일>.minisig`)을, 앱에 들어 있는 공개 키
//! (`packaging/usagi-update.pub`)로 확인합니다. 서명 안의 설명(버전 + 파일 이름)도 맞아야 해서
//! 예전 버전의 서명된 파일을 새 버전인 것처럼 내놓을 수 없어요.

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
    /// 같은 이름 + .minisig (서명). 이 빌드에 공개 키가 있으면 꼭 있어야 함
    pub sig_url: Option<String>,
}

/// 저장소의 서명 공개 키 파일 (minisign 형식: 설명 한 줄 + 키 한 줄). `usagi-sign keygen`이 만들어 넣음
const KEY_FILE: &str = include_str!("../../../packaging/usagi-update.pub");

/// 이 빌드에 들어 있는 서명 공개 키 (base64 한 줄). 아직 만들지 않았으면 None (그때는 체크섬만 확인)
pub fn signing_key() -> Option<&'static str> {
    key_line(KEY_FILE)
}

fn key_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("untrusted comment:") && !l.starts_with('#'))
}

/// 서명에 넣는 설명 (서명에 포함되어 바꿀 수 없음): 버전과 파일 이름
pub fn signed_comment(version: &str, asset: &str) -> String {
    format!("usAgI {version} {asset}")
}

/// 받은 파일의 서명 확인: 공개 키로 만든 서명이 맞고, 서명 안의 설명이 이 버전·이 파일이어야 함
pub fn verify_signature(key_b64: &str, data: &[u8], signature: &str, version: &str, asset: &str) -> Result<(), String> {
    use minisign_verify::{PublicKey, Signature};
    let bad = || crate::tr!("업데이트 서명 확인 실패. 설치 중단", "Update signature check failed. Not installing").to_string();
    let key = PublicKey::from_base64(key_b64).map_err(|_| bad())?;
    let sig = Signature::decode(signature).map_err(|_| bad())?;
    key.verify(data, &sig, false).map_err(|_| bad())?;
    if sig.trusted_comment() != signed_comment(version, asset) {
        return Err(crate::tr!("다른 버전이나 다른 파일의 서명. 설치 중단", "The signature belongs to another version or file. Not installing").into());
    }
    Ok(())
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
/// 받을 주소는 이 저장소(`repo`)의 릴리스 파일 주소여야 해요. 이 빌드에 서명 공개 키가 있으면 서명 파일도 있어야 해요.
pub fn parse_latest(body: &[u8], current: &str, repo: &str, asset: &str) -> Result<Option<Release>, String> {
    parse_latest_with(body, current, repo, asset, signing_key().is_some())
}

/// `require_signature`: 서명 파일(`<asset>.minisig`)이 없는 릴리스를 받지 않음
pub fn parse_latest_with(body: &[u8], current: &str, repo: &str, asset: &str, require_signature: bool) -> Result<Option<Release>, String> {
    let v: Value =
        serde_json::from_slice(body).map_err(|_| crate::tr!("릴리스 정보 읽기 실패", "Failed to read the release info").to_string())?;
    let tag = v.get("tag_name").and_then(|t| t.as_str()).unwrap_or("");
    if !version::is_newer(tag, current) {
        return Ok(None);
    }
    let version = version::clean(tag);
    if !safe_version(&version) {
        return Err(crate::tr!("릴리스 버전 형식 오류", "Invalid release version format").into());
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
    let asset_url = url_of(asset)
        .ok_or_else(|| crate::tr!(format!("새 릴리스에 {asset} 파일 없음"), format!("The new release has no {asset} file")))?;
    let sha_url = url_of(&format!("{asset}.sha256")).ok_or_else(|| {
        crate::tr!(format!("새 릴리스에 {asset}.sha256 (체크섬) 없음"), format!("The new release has no {asset}.sha256 (checksum)"))
    })?;
    let sig_url = url_of(&format!("{asset}.minisig"));
    if require_signature && sig_url.is_none() {
        // 릴리스 게시 뒤 서명을 올리기 전일 수 있음
        return Err(crate::tr!(
            format!("v{version} 서명 파일 없음 (게시 직후면 서명 올리는 중). 잠시 뒤 다시 확인"),
            format!("v{version} has no signature file yet (it may still be uploading). Check again later")
        ));
    }
    Ok(Some(Release {
        version,
        page: v.get("html_url").and_then(|u| u.as_str()).unwrap_or("").to_string(),
        notes: v.get("body").and_then(|b| b.as_str()).unwrap_or("").to_string(),
        asset_url,
        sha_url,
        sig_url,
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
        let r = parse_latest_with(BODY.as_bytes(), "0.1.0", "o/r", "usagi.exe", false).unwrap().unwrap();
        assert_eq!(r.version, "0.2.0");
        assert!(r.asset_url.ends_with("/usagi.exe"));
        assert!(r.sha_url.ends_with(".sha256"));
    }

    #[test]
    fn same_version_is_none() {
        assert_eq!(parse_latest_with(BODY.as_bytes(), "0.2.0", "o/r", "usagi.exe", false).unwrap(), None);
    }

    #[test]
    fn missing_asset_is_error() {
        assert!(parse_latest_with(BODY.as_bytes(), "0.1.0", "o/r", "usAgI-mac.zip", false).is_err());
    }

    #[test]
    fn rejects_other_hosts_missing_checksum_and_odd_versions() {
        // 다른 저장소·다른 서버의 파일 주소는 받지 않음
        assert!(parse_latest_with(BODY.as_bytes(), "0.1.0", "someone/else", "usagi.exe", false).is_err());
        let evil = BODY.replace("https://github.com/o/r/releases/download/v0.2.0/usagi.exe\"", "https://evil.example/usagi.exe\"");
        assert!(parse_latest_with(evil.as_bytes(), "0.1.0", "o/r", "usagi.exe", false).is_err());
        // 체크섬 파일이 없으면 받지 않음
        let no_sha = BODY.replace("usagi.exe.sha256\",\"browser", "other.txt\",\"browser");
        assert!(parse_latest_with(no_sha.as_bytes(), "0.1.0", "o/r", "usagi.exe", false).is_err());
        // 버전이 폴더 이름으로 쓰이므로 경로 글자는 안 됨
        let odd = BODY.replace("\"tag_name\":\"v0.2.0\"", "\"tag_name\":\"v0.2.0-../../x\"");
        assert!(parse_latest_with(odd.as_bytes(), "0.1.0", "o/r", "usagi.exe", false).is_err());
        let pre = BODY.replace("\"tag_name\":\"v0.2.0\"", "\"tag_name\":\"v0.2.0-beta.1\"");
        assert_eq!(parse_latest_with(pre.as_bytes(), "0.1.0", "o/r", "usagi.exe", false).unwrap().unwrap().version, "0.2.0-beta.1");
    }

    #[test]
    fn checksum_file_format() {
        let h = "b4c4c0d84196b00ba360934efae3c67d683ebf23c01af88124fbaa742be4f864";
        assert_eq!(parse_sha256(&format!("{h}  usagi.exe\n")).as_deref(), Some(h));
        assert_eq!(parse_sha256(&h.to_uppercase()).as_deref(), Some(h));
        assert_eq!(parse_sha256("<html>Not Found</html>"), None);
        assert_eq!(parse_sha256(""), None);
    }

    #[test]
    fn signature_file_is_required_when_a_key_is_built_in() {
        // 서명 파일이 없는 릴리스: 키가 있는 빌드는 받지 않음 (없는 빌드는 체크섬만)
        assert!(parse_latest_with(BODY.as_bytes(), "0.1.0", "o/r", "usagi.exe", true).unwrap_err().contains("서명"));
        assert_eq!(parse_latest_with(BODY.as_bytes(), "0.1.0", "o/r", "usagi.exe", false).unwrap().unwrap().sig_url, None);
        let signed = BODY.replace(
            r#""assets":["#,
            r#""assets":[{"name":"usagi.exe.minisig","browser_download_url":"https://github.com/o/r/releases/download/v0.2.0/usagi.exe.minisig"},"#,
        );
        let r = parse_latest_with(signed.as_bytes(), "0.1.0", "o/r", "usagi.exe", true).unwrap().unwrap();
        assert!(r.sig_url.unwrap().ends_with("/usagi.exe.minisig"));
        // 다른 서버의 서명 주소는 없는 것으로 봄
        let evil = signed.replace("https://github.com/o/r/releases/download/v0.2.0/usagi.exe.minisig", "https://evil.example/usagi.exe.minisig");
        assert!(parse_latest_with(evil.as_bytes(), "0.1.0", "o/r", "usagi.exe", true).is_err());
        // 같은 버전이면 서명 유무와 상관없이 업데이트 없음
        assert_eq!(parse_latest_with(BODY.as_bytes(), "0.2.0", "o/r", "usagi.exe", true).unwrap(), None);
    }

    /// 테스트용 키로 서명 (비밀번호 없는 키). 돌려주는 값: (공개 키 base64, 서명 글자)
    fn sign_for_test(data: &[u8], comment: &str) -> (String, String) {
        let minisign::KeyPair { pk, sk } = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        let sig = minisign::sign(Some(&pk), &sk, std::io::Cursor::new(data), Some(comment), Some("test")).unwrap();
        (pk.to_base64(), sig.into_string())
    }

    #[test]
    fn signature_must_match_key_data_version_and_file() {
        let data = b"MZ new usagi.exe bytes";
        let (key, sig) = sign_for_test(data, &signed_comment("0.2.1", "usagi.exe"));
        assert_eq!(verify_signature(&key, data, &sig, "0.2.1", "usagi.exe"), Ok(()));
        // 내용이 한 글자라도 다르면 실패
        assert!(verify_signature(&key, b"MZ new usagi.exe byteZ", &sig, "0.2.1", "usagi.exe").is_err());
        // 예전 버전의 서명된 파일을 새 버전으로 내놓기, 다른 파일의 서명 쓰기
        assert!(verify_signature(&key, data, &sig, "0.3.0", "usagi.exe").is_err());
        assert!(verify_signature(&key, data, &sig, "0.2.1", "usAgI-mac.zip").is_err());
        // 다른 키로 만든 서명
        let (other_key, other_sig) = sign_for_test(data, &signed_comment("0.2.1", "usagi.exe"));
        assert!(verify_signature(&key, data, &other_sig, "0.2.1", "usagi.exe").is_err());
        assert!(verify_signature(&other_key, data, &sig, "0.2.1", "usagi.exe").is_err());
        // 서명 안의 설명만 바꿔 넣기 (설명도 서명에 포함)
        let forged = sig.replace("usAgI 0.2.1 usagi.exe", "usAgI 0.9.9 usagi.exe");
        assert!(verify_signature(&key, data, &forged, "0.9.9", "usagi.exe").is_err());
        // 형식이 아닌 값
        assert!(verify_signature(&key, data, "<html>Not Found</html>", "0.2.1", "usagi.exe").is_err());
        assert!(verify_signature("not a key", data, &sig, "0.2.1", "usagi.exe").is_err());
    }

    #[test]
    fn key_file_placeholder_means_no_key() {
        assert_eq!(key_line("untrusted comment: usAgI update signing key - not created yet\n"), None);
        assert_eq!(key_line(""), None);
        assert_eq!(key_line("untrusted comment: minisign public key ABC\nRWQabc123\n"), Some("RWQabc123"));
        // 저장소의 키 파일은 비어 있거나 올바른 키여야 함 (잘못된 키가 들어가면 모든 업데이트가 막힘)
        if let Some(key) = signing_key() {
            assert!(minisign_verify::PublicKey::from_base64(key).is_ok(), "packaging/usagi-update.pub 형식 오류");
        }
    }
}
