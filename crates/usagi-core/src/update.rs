//! 자동 업데이트 공통 부분: GitHub 릴리스 확인·받기([`check`], [`download`], [`fetch_text`]), 받을 파일 고르기, 서명 검증.
//! 받은 파일을 두는 곳과 바꿔 끼우기는 플랫폼마다 달라서 각 앱의 updater가 해요.
//!
//! 서명: 체크섬은 실행 파일과 같은 릴리스에서 오므로 릴리스를 바꿔치기한 쪽이 함께 바꿀 수 있어요.
//! 그래서 저장소에 없는 비밀 키(릴리스 워크플로의 비밀값)로 만든 minisign 서명(`<파일>.minisig`)을, 앱에 들어 있는 공개 키
//! (`packaging/usagi-update.pub`)로 확인합니다. 서명 안의 설명(버전 + 파일 이름)도 맞아야 해서
//! 예전 버전의 서명된 파일을 새 버전인 것처럼 내놓을 수 없어요.

use crate::version;
use minisign_verify::{PublicKey, Signature};
use serde_json::Value;
use std::io::{self, Read};

mod download;
pub use download::{check, download, fetch_text, REPO};

pub const MAX_ASSET_BYTES: u64 = 64 * 1024 * 1024;

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
    pub verification: Verification,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Verification {
    Body { checksum: String, signature: Option<String> },
    Attachments { sha_url: String, sig_url: Option<String> },
}

impl Verification {
    pub fn checksum(&self, fetch: impl FnOnce(&str) -> Result<String, String>) -> Result<String, String> {
        match self {
            Self::Body { checksum, .. } => Ok(checksum.clone()),
            Self::Attachments { sha_url, .. } => parse_sha256(&fetch(sha_url)?)
                .ok_or_else(|| crate::tr!("체크섬 수신 실패", "Couldn't download the checksum").into()),
        }
    }

    fn signature(&self, fetch: impl FnOnce(&str) -> Result<String, String>) -> Result<String, String> {
        let missing = || crate::tr!("서명 수신 실패. 설치 중단", "Couldn't download the signature. Not installing").to_string();
        match self {
            Self::Body { signature, .. } => signature.clone().ok_or_else(missing),
            Self::Attachments { sig_url, .. } => fetch(sig_url.as_deref().ok_or_else(missing)?).map_err(|_| missing()),
        }
    }
}

impl Release {
    pub fn verify_asset(
        &self,
        reader: impl Read,
        asset: &str,
        fetch: impl Fn(&str) -> Result<String, String>,
    ) -> Result<(), String> {
        let checksum = self.verification.checksum(&fetch)?;
        let signed = signing_key()
            .map(|key| {
                let signature = self.verification.signature(&fetch)?;
                decode_signature(key, &signature)
            })
            .transpose()?;
        verify_reader(reader, &checksum, signed.as_ref(), &self.version, asset)
    }
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
    let (key, sig) = decode_signature(key_b64, signature)?;
    key.verify(data, &sig, false).map_err(|_| invalid_signature())?;
    verify_comment(&sig, version, asset)
}

fn invalid_signature() -> String {
    crate::tr!("업데이트 서명 확인 실패. 설치 중단", "Update signature check failed. Not installing").into()
}

fn decode_signature(key_b64: &str, signature: &str) -> Result<(PublicKey, Signature), String> {
    let key = PublicKey::from_base64(key_b64).map_err(|_| invalid_signature())?;
    let sig = Signature::decode(signature).map_err(|_| invalid_signature())?;
    Ok((key, sig))
}

fn verify_comment(sig: &Signature, version: &str, asset: &str) -> Result<(), String> {
    if sig.trusted_comment() != signed_comment(version, asset) {
        return Err(crate::tr!("다른 버전이나 다른 파일의 서명. 설치 중단", "The signature belongs to another version or file. Not installing").into());
    }
    Ok(())
}

fn verify_reader(
    reader: impl Read,
    checksum: &str,
    signed: Option<&(PublicKey, Signature)>,
    version: &str,
    asset: &str,
) -> Result<(), String> {
    let mut reader = reader.take(MAX_ASSET_BYTES + 1);
    let mut sha = crate::sha256::Sha256::default();
    let mut signature = signed
        .map(|(key, sig)| key.verify_stream(sig).map_err(|_| invalid_signature()))
        .transpose()?;
    let mut remaining = MAX_ASSET_BYTES;
    let mut buf = [0; 64 * 1024];
    loop {
        let count = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.to_string()),
        };
        remaining = remaining.checked_sub(count as u64)
            .ok_or_else(|| crate::tr!("파일 크기 초과", "File too large").to_string())?;
        sha.update(&buf[..count]);
        if let Some(verifier) = &mut signature {
            verifier.update(&buf[..count]);
        }
    }
    if sha.finish_hex() != checksum {
        return Err(crate::tr!("받은 파일 손상 (체크섬 불일치). 다시 시도 필요", "Downloaded file is damaged (checksum mismatch). Try again").into());
    }
    if let (Some(mut verifier), Some((_, sig))) = (signature, signed) {
        verifier.finalize().map_err(|_| invalid_signature())?;
        verify_comment(sig, version, asset)?;
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

const VERIFICATION_START: &str = "<!-- usagi-verification:start -->";
const VERIFICATION_END: &str = "<!-- usagi-verification:end -->";

fn body_verification(body: &str) -> Result<Option<(&str, &str, &str)>, String> {
    if !body.contains(VERIFICATION_START) && !body.contains(VERIFICATION_END) {
        return Ok(None);
    }
    let (before, section) = body.split_once(VERIFICATION_START).ok_or_else(invalid_verification)?;
    let (block, after) = section.split_once(VERIFICATION_END).ok_or_else(invalid_verification)?;
    if before.contains(VERIFICATION_END) || section.contains(VERIFICATION_START) || after.contains(VERIFICATION_END) {
        return Err(invalid_verification());
    }
    Ok(Some((before, block, after)))
}

fn invalid_verification() -> String {
    crate::tr!("릴리스 본문의 검증 정보 형식 오류", "Invalid verification information in release notes").into()
}

fn fenced_text(block: &str, heading: &str) -> Option<String> {
    let mut lines = block.lines();
    lines.find(|line| *line == heading)?;
    if lines.find(|line| !line.trim().is_empty())? != "```text" {
        return None;
    }
    let mut value = String::new();
    for line in lines {
        if line == "```" {
            return (!value.trim().is_empty()).then_some(value);
        }
        value.push_str(line);
        value.push('\n');
    }
    None
}

fn body_checksum(block: &str, asset: &str) -> Option<String> {
    let checksums = fenced_text(block, "### SHA-256")?;
    let mut matches = checksums.lines().filter(|line| {
        let mut fields = line.split_whitespace();
        fields.next();
        fields.next() == Some(asset)
    });
    let line = matches.next()?;
    if matches.next().is_some() || line.split_whitespace().count() != 2 {
        return None;
    }
    parse_sha256(line)
}

/// GitHub의 접힌 검증 정보는 앱의 변경 내용에 표시하지 않음.
fn display_notes(before: &str, after: &str) -> String {
    match (before.trim_end(), after.trim_start()) {
        ("", notes) | (notes, "") => notes.to_owned(),
        (before, after) => format!("{before}\n\n{after}"),
    }
}

/// `releases/latest` 응답을 읽음. 지금(`current`)보다 새 버전이고 `asset` 파일과 체크섬이 있으면 Some.
/// 받을 주소는 이 저장소(`repo`)의 릴리스 파일 주소여야 해요. 공개 키가 들어 있는 빌드는 서명도 필요해요.
pub fn parse_latest(body: &[u8], current: &str, repo: &str, asset: &str) -> Result<Option<Release>, String> {
    parse_latest_with(body, current, repo, asset, signing_key().is_some())
}

/// `require_signature`: 본문이나 첨부에 서명이 없는 릴리스를 받지 않음
pub fn parse_latest_with(body: &[u8], current: &str, repo: &str, asset: &str, require_signature: bool) -> Result<Option<Release>, String> {
    let v: Value =
        serde_json::from_slice(body).map_err(|_| crate::tr!("릴리스 정보 읽기 실패", "Failed to read the release info").to_string())?;
    let tag = v.get("tag_name").and_then(|t| t.as_str()).unwrap_or("");
    let version = version::clean(tag);
    if !tag.is_empty() && !safe_version(&version) {
        return Err(crate::tr!("릴리스 버전 형식 오류", "Invalid release version format").into());
    }
    if !version::is_newer(tag, current) {
        return Ok(None);
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
    let body = v.get("body").and_then(Value::as_str).unwrap_or("");
    let missing_signature = || crate::tr!(
        format!("v{version} 업데이트 서명 없음. 잠시 뒤 다시 확인"),
        format!("v{version} has no update signature. Check again later")
    );
    let (verification, notes) = if let Some((before, block, after)) = body_verification(body)? {
        let checksum = body_checksum(block, asset).ok_or_else(invalid_verification)?;
        let signature = fenced_text(block, &format!("**`{asset}.minisig`**"));
        if require_signature && signature.is_none() {
            return Err(missing_signature());
        }
        (Verification::Body { checksum, signature }, display_notes(before, after))
    } else {
        let sha_url = url_of(&format!("{asset}.sha256")).ok_or_else(|| {
            crate::tr!(format!("새 릴리스에 {asset}.sha256 (체크섬) 없음"), format!("The new release has no {asset}.sha256 (checksum)"))
        })?;
        let sig_url = url_of(&format!("{asset}.minisig"));
        if require_signature && sig_url.is_none() {
            return Err(missing_signature());
        }
        (Verification::Attachments { sha_url, sig_url }, body.to_owned())
    };
    Ok(Some(Release {
        version,
        page: v.get("html_url").and_then(|u| u.as_str()).unwrap_or("").to_string(),
        notes,
        asset_url,
        verification,
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
    const HASH: &str = "b4c4c0d84196b00ba360934efae3c67d683ebf23c01af88124fbaa742be4f864";

    #[test]
    fn newer_release_with_asset() {
        let r = parse_latest_with(BODY.as_bytes(), "0.1.0", "o/r", "usagi.exe", false).unwrap().unwrap();
        assert_eq!(r.version, "0.2.0");
        assert!(r.asset_url.ends_with("/usagi.exe"));
        let checksum = r.verification.checksum(|url| {
            assert!(url.ends_with("/usagi.exe.sha256"));
            Ok(format!("{HASH}  usagi.exe\n"))
        }).unwrap();
        assert_eq!(checksum, HASH);
    }

    #[test]
    fn hides_release_verification_block_from_update_notes() {
        let mut response: Value = serde_json::from_str(BODY).unwrap();
        response["body"] = Value::String(format!(
            "### 변경 내용\r\n\r\n{VERIFICATION_START}\r\n<details>\r\n\r\n### SHA-256\r\n\r\n```text\r\n{HASH}  usagi.exe\r\n```\r\n</details>\r\n{VERIFICATION_END}\r\n\r\n추가 안내"
        ));
        let release = parse_latest_with(&serde_json::to_vec(&response).unwrap(), "0.1.0", "o/r", "usagi.exe", false).unwrap().unwrap();
        assert_eq!(release.notes, "### 변경 내용\n\n추가 안내");
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
        let unsigned = parse_latest_with(BODY.as_bytes(), "0.1.0", "o/r", "usagi.exe", false).unwrap().unwrap();
        assert!(matches!(unsigned.verification, Verification::Attachments { sig_url: None, .. }));
        let signed = BODY.replace(
            r#""assets":["#,
            r#""assets":[{"name":"usagi.exe.minisig","browser_download_url":"https://github.com/o/r/releases/download/v0.2.0/usagi.exe.minisig"},"#,
        );
        let r = parse_latest_with(signed.as_bytes(), "0.1.0", "o/r", "usagi.exe", true).unwrap().unwrap();
        assert!(matches!(r.verification, Verification::Attachments { sig_url: Some(url), .. } if url.ends_with("/usagi.exe.minisig")));
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
    fn body_only_release_uses_the_checksum_and_signature_for_its_asset() {
        let data = b"release fixture";
        let checksum = crate::sha256::hex(data);
        for (asset, other) in [("usagi.exe", "usAgI-mac.zip"), ("usAgI-mac.zip", "usagi.exe")] {
            let (key, sig) = sign_for_test(data, &signed_comment("0.3.1", asset));
            let response = serde_json::json!({
                "tag_name": "v0.3.1",
                "body": format!("Notes\n\n{VERIFICATION_START}\n<details>\n\n### SHA-256\n\n```text\n{HASH}  {other}\n{checksum}  {asset}\n```\n\n### Minisign\n\n**`{asset}.minisig`**\n\n```text\n{sig}```\n</details>\n{VERIFICATION_END}"),
                "assets": [{"name": asset, "browser_download_url": format!("https://github.com/o/r/releases/download/v0.3.1/{asset}")}],
            });
            let release = parse_latest_with(&serde_json::to_vec(&response).unwrap(), "0.3.0", "o/r", asset, true).unwrap().unwrap();
            assert_eq!(release.verification.checksum(|_| panic!("unexpected checksum download")).unwrap(), checksum);
            let signature = release.verification.signature(|_| panic!("unexpected signature download")).unwrap();
            assert_eq!(verify_signature(&key, data, &signature, &release.version, asset), Ok(()));
            assert!(verify_signature(&key, b"changed fixture", &signature, &release.version, asset).is_err());
            assert_eq!(release.notes, "Notes");
        }
    }

    #[test]
    fn invalid_body_verification_does_not_fall_back_to_attachments() {
        let mut response: Value = serde_json::from_str(BODY).unwrap();
        for text in [
            format!("{VERIFICATION_START}\nunfinished"),
            format!("{VERIFICATION_START}\n### SHA-256\n```text\n{HASH}  usagi.exe\n{HASH}  usagi.exe\n```\n{VERIFICATION_END}"),
        ] {
            response["body"] = Value::String(text);
            assert!(parse_latest_with(&serde_json::to_vec(&response).unwrap(), "0.1.0", "o/r", "usagi.exe", false).is_err());
        }
        response["body"] = Value::String(format!(
            "{VERIFICATION_START}\n### SHA-256\n```text\n{HASH}  usagi.exe\n```\n{VERIFICATION_END}"
        ));
        assert!(parse_latest_with(&serde_json::to_vec(&response).unwrap(), "0.1.0", "o/r", "usagi.exe", true).unwrap_err().contains("서명"));
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
    fn verifies_streamed_package_and_rejects_tampering() {
        let mut data = vec![b'X'; 3 * 64 * 1024 + 63];
        let asset = "usAgI-mac.zip";
        let version = "0.3.1";
        let checksum = crate::sha256::hex(&data);
        let (key, sig) = sign_for_test(&data, &signed_comment(version, asset));
        let signature = decode_signature(&key, &sig).unwrap();
        assert_eq!(verify_reader(&data[..], &checksum, Some(&signature), version, asset), Ok(()));
        assert!(verify_reader(&data[..], &checksum, Some(&signature), version, "usagi.exe").is_err());
        data[64 * 1024] ^= 1;
        assert!(verify_reader(&data[..], &checksum, Some(&signature), version, asset).is_err());
        let forged_checksum = crate::sha256::hex(&data);
        assert!(verify_reader(&data[..], &forged_checksum, Some(&signature), version, asset).is_err());
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
