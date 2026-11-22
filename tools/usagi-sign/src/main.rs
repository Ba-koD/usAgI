//! usAgI 릴리스 서명 도구 (minisign 형식, 공식 minisign 프로그램으로도 확인 가능).
//!
//!   usagi-sign keygen [--key <비밀 키 파일>]
//!       서명 키를 새로 만듦. 비밀 키는 비밀번호로 잠가 저장소 밖에 두고, 공개 키는 packaging/usagi-update.pub 에 씀.
//!   usagi-sign sign --version <버전> [--key <비밀 키 파일>] <파일>...
//!       각 파일 옆에 <파일>.minisig 를 만듦. 서명 안에 "usAgI <버전> <파일 이름>"이 들어감.
//!   usagi-sign verify --version <버전> <파일>...
//!       <파일>.minisig 를 저장소의 공개 키로 확인.
//!
//! 비밀번호는 이 도구가 직접 물어봄 (화면에 보이지 않음). 환경 변수 `USAGI_SIGN_PASSWORD`가 있으면 그 값을 씀 (자동 실행용).
//! 비밀 키 기본 위치: 홈 폴더의 `.usagi/update-signing.key`

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use minisign::{KeyPair, PublicKey, SecretKeyBox};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("keygen") => keygen(&args[1..]),
        Some("sign") => sign(&args[1..]),
        Some("verify") => verify(&args[1..]),
        _ => Err("사용: usagi-sign keygen | sign --version <버전> <파일>... | verify --version <버전> <파일>...".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("오류: {e}");
            ExitCode::FAILURE
        }
    }
}

/// 저장소의 공개 키 파일
fn public_key_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/usagi-update.pub")
}

/// 비밀 키 기본 위치 (저장소 밖)
fn default_key_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).ok_or("홈 폴더 찾기 실패")?;
    Ok(PathBuf::from(home).join(".usagi").join("update-signing.key"))
}

/// `--이름 값` 꺼내기. 남은 인자는 파일 목록
fn take(args: &[String], name: &str) -> (Option<String>, Vec<String>) {
    let mut value = None;
    let mut rest = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == name {
            value = it.next().cloned();
        } else {
            rest.push(a.clone());
        }
    }
    (value, rest)
}

/// 공개 키 파일에서 키 한 줄 (없으면 None)
fn repo_key() -> Option<String> {
    let text = std::fs::read_to_string(public_key_path()).ok()?;
    text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("untrusted comment:") && !l.starts_with('#')).map(str::to_string)
}

fn keygen(args: &[String]) -> Result<(), String> {
    let (key, _) = take(args, "--key");
    let secret = key.map(PathBuf::from).map_or_else(default_key_path, Ok)?;
    if secret.exists() {
        return Err(format!("비밀 키가 이미 있음: {}\n   새로 만들려면 그 파일을 직접 옮기거나 지운 뒤 다시 실행", secret.display()));
    }
    if repo_key().is_some() {
        return Err(format!(
            "저장소에 공개 키가 이미 있음: {}\n   키를 바꾸면 예전 키가 들어 있는 설치본은 새 버전을 받지 못함. 정말 바꾸려면 그 파일의 키 줄을 지운 뒤 다시 실행",
            public_key_path().display()
        ));
    }
    println!("새 서명 키 생성. 비밀 키를 잠글 비밀번호 입력 (화면에 보이지 않음, 두 번):");
    let KeyPair { pk, sk } = KeyPair::generate_encrypted_keypair(None).map_err(|e| e.to_string())?;
    let dir = secret.parent().ok_or("비밀 키 폴더 경로 오류")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // 비밀 키 폴더는 이 사용자만
    usagi_core::private_dir::make_private(dir);
    let boxed = sk.to_box(Some("usAgI update signing secret key")).map_err(|e| e.to_string())?;
    std::fs::write(&secret, boxed.into_string()).map_err(|e| e.to_string())?;
    let public = pk.to_box().map_err(|e| e.to_string())?.into_string();
    std::fs::write(public_key_path(), public.trim_end().to_string() + "\n").map_err(|e| e.to_string())?;
    println!();
    println!("비밀 키: {}", secret.display());
    println!("   저장소 밖에 보관. 이 파일과 비밀번호를 잃으면 이 키로는 더 서명할 수 없으므로 안전한 곳에 백업 필요.");
    println!("공개 키: {}", public_key_path().display());
    println!("   {}", pk.to_base64());
    println!("   이 파일을 커밋하면 다음 빌드부터 앱이 서명 없는 업데이트를 받지 않음.");
    println!("GitHub Actions에서 서명하려면 비밀값 등록: gh secret set UPDATE_SIGNING_KEY < <비밀 키 파일>");
    Ok(())
}

/// 비밀 키 읽기 (비밀번호를 물어봄). 저장소의 공개 키와 짝이 맞는지도 확인
fn load_secret(key: Option<String>) -> Result<(PublicKey, minisign::SecretKey, String), String> {
    let path = key.map(PathBuf::from).map_or_else(default_key_path, Ok)?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("비밀 키 읽기 실패 ({}): {e}", path.display()))?;
    let repo = repo_key().ok_or("저장소에 공개 키 없음. 먼저: cargo run -p usagi-sign -- keygen")?;
    // 자동 실행(GitHub Actions)에서는 환경 변수 USAGI_SIGN_PASSWORD 값을 씀 (빈 값이면 빈 비밀번호). 없으면 직접 물어봄
    let password = std::env::var("USAGI_SIGN_PASSWORD").ok();
    if password.is_none() {
        println!("서명 키 비밀번호 입력 (화면에 보이지 않음, 비밀번호 없이 만든 키면 Enter):");
    }
    let sk = SecretKeyBox::from_string(&text).map_err(|e| e.to_string())?.into_secret_key(password).map_err(|_| "비밀번호가 틀렸거나 키 파일 손상")?;
    let pk = PublicKey::from_secret_key(&sk).map_err(|e| e.to_string())?;
    if pk.to_base64() != repo {
        return Err("이 비밀 키는 저장소의 공개 키(packaging/usagi-update.pub)와 짝이 아님".into());
    }
    Ok((pk, sk, repo))
}

fn file_name(path: &Path) -> Result<String, String> {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or_else(|| format!("파일 이름 없음: {}", path.display()))
}

fn sign(args: &[String]) -> Result<(), String> {
    let (version, rest) = take(args, "--version");
    let (key, files) = take(&rest, "--key");
    let version = version.ok_or("--version <버전> 필요")?;
    let version = version.trim_start_matches(['v', 'V']);
    if files.is_empty() {
        return Err("서명할 파일 필요".into());
    }
    let (pk, sk, repo) = load_secret(key)?;
    sign_files(&pk, &sk, &repo, version, &files)
}

/// 파일마다 `<파일>.minisig` 를 만들고, 앱이 쓰는 것과 같은 검증으로 바로 확인
fn sign_files(pk: &PublicKey, sk: &minisign::SecretKey, key_b64: &str, version: &str, files: &[String]) -> Result<(), String> {
    for f in files {
        let path = Path::new(f);
        let name = file_name(path)?;
        let data = std::fs::read(path).map_err(|e| format!("{name} 읽기 실패: {e}"))?;
        let comment = usagi_core::update::signed_comment(version, &name);
        let sig = minisign::sign(Some(pk), sk, std::io::Cursor::new(&data), Some(&comment), Some("usAgI release signature (minisign)"))
            .map_err(|e| e.to_string())?
            .into_string();
        usagi_core::update::verify_signature(key_b64, &data, &sig, version, &name)?;
        let out = PathBuf::from(format!("{f}.minisig"));
        std::fs::write(&out, sig).map_err(|e| e.to_string())?;
        println!("서명 완료: {name}.minisig  ({comment})");
    }
    Ok(())
}

fn verify(args: &[String]) -> Result<(), String> {
    let (version, files) = take(args, "--version");
    let version = version.ok_or("--version <버전> 필요")?;
    let version = version.trim_start_matches(['v', 'V']);
    let repo = repo_key().ok_or("저장소에 공개 키 없음")?;
    if files.is_empty() {
        return Err("확인할 파일 필요".into());
    }
    verify_files(&repo, version, &files)
}

fn verify_files(key_b64: &str, version: &str, files: &[String]) -> Result<(), String> {
    for f in files {
        let path = Path::new(f);
        let name = file_name(path)?;
        let data = std::fs::read(path).map_err(|e| format!("{name} 읽기 실패: {e}"))?;
        let sig = std::fs::read_to_string(format!("{f}.minisig")).map_err(|e| format!("{name}.minisig 읽기 실패: {e}"))?;
        usagi_core::update::verify_signature(key_b64, &data, &sig, version, &name).map_err(|e| format!("{name}: {e}"))?;
        println!("서명 확인: {name}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 테스트 전용 임시 폴더 (끝나면 지움)
    struct Temp(PathBuf);
    impl Temp {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("usagi-sign-test-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Temp(dir)
        }
        fn file(&self, name: &str, body: &[u8]) -> String {
            let p = self.0.join(name);
            std::fs::write(&p, body).unwrap();
            p.to_string_lossy().into_owned()
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn signs_release_files_and_the_app_check_accepts_them() {
        let tmp = Temp::new("sign");
        let KeyPair { pk, sk } = KeyPair::generate_unencrypted_keypair().unwrap();
        let key = pk.to_base64();
        let files = vec![tmp.file("usagi.exe", b"MZ windows build"), tmp.file("usAgI-mac.zip", b"PK mac build")];
        sign_files(&pk, &sk, &key, "0.2.1", &files).unwrap();
        assert!(Path::new(&format!("{}.minisig", files[0])).is_file());
        verify_files(&key, "0.2.1", &files).unwrap();
        // 다른 버전으로는 통과하지 않음
        assert!(verify_files(&key, "0.3.0", &files).is_err());
        // 서명 뒤에 파일이 바뀌면 통과하지 않음
        std::fs::write(&files[0], b"MZ tampered build").unwrap();
        assert!(verify_files(&key, "0.2.1", &files[..1]).is_err());
        verify_files(&key, "0.2.1", &files[1..]).unwrap();
        // 서명 파일을 서로 바꿔 끼워도 통과하지 않음
        std::fs::copy(format!("{}.minisig", files[1]), format!("{}.minisig", files[0])).unwrap();
        assert!(verify_files(&key, "0.2.1", &files[..1]).is_err());
    }

    #[test]
    fn password_protected_key_round_trip() {
        let tmp = Temp::new("pw");
        let KeyPair { pk, sk } = KeyPair::generate_encrypted_keypair(Some("correct horse".into())).unwrap();
        let stored = sk.to_box(Some("test key")).unwrap().into_string();
        // 틀린 비밀번호로는 열리지 않음
        assert!(SecretKeyBox::from_string(&stored).unwrap().into_secret_key(Some("wrong".into())).is_err());
        let opened = SecretKeyBox::from_string(&stored).unwrap().into_secret_key(Some("correct horse".into())).unwrap();
        assert_eq!(PublicKey::from_secret_key(&opened).unwrap().to_base64(), pk.to_base64());
        let files = vec![tmp.file("usagi.exe", b"MZ build")];
        sign_files(&pk, &opened, &pk.to_base64(), "1.0.0", &files).unwrap();
        verify_files(&pk.to_base64(), "1.0.0", &files).unwrap();
    }
}
