//! 이 사용자만 쓰는 폴더 만들기.
//!
//! usAgI가 계정마다 만드는 프로필 폴더에는 공식 CLI가 자기 로그인 파일을 둡니다 (usAgI는 그 파일을 읽지 않음).
//! 상위 폴더의 권한을 물려받지 않게 해서, 다른 사용자·샌드박스 계정·앱 컨테이너가 그 파일을 읽지 못하게 합니다.
//! - Windows: 상속을 끊고 지금 사용자·SYSTEM·관리자만 허용 (하위 폴더·파일에 상속)
//! - 그 외: 권한 0700

use std::path::Path;

/// 폴더를 이 사용자 전용으로 바꿈. 실패해도 앱 동작에는 영향 없음 (false 반환)
pub fn make_private(dir: &Path) -> bool {
    imp::make_private(dir)
}

#[cfg(unix)]
mod imp {
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    pub fn make_private(dir: &Path) -> bool {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).is_ok()
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SetNamedSecurityInfoW, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorDacl, GetTokenInformation, TokenUser, ACL, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// 지금 사용자의 SID 문자열 ("S-1-5-21-…")
    fn user_sid() -> Option<String> {
        unsafe {
            let mut token: HANDLE = null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return None;
            }
            // TOKEN_USER + SID (최대 68바이트). 포인터 정렬을 맞추려고 u64 배열
            let mut buf = [0u64; 32];
            let mut len = 0u32;
            let ok = GetTokenInformation(token, TokenUser, buf.as_mut_ptr().cast(), std::mem::size_of_val(&buf) as u32, &mut len);
            CloseHandle(token);
            if ok == 0 {
                return None;
            }
            let user = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut text: *mut u16 = null_mut();
            if ConvertSidToStringSidW(user.User.Sid, &mut text) == 0 || text.is_null() {
                return None;
            }
            let mut n = 0;
            while *text.add(n) != 0 {
                n += 1;
            }
            let sid = String::from_utf16_lossy(std::slice::from_raw_parts(text, n));
            LocalFree(text.cast());
            Some(sid)
        }
    }

    pub fn make_private(dir: &Path) -> bool {
        let Some(sid) = user_sid() else { return false };
        // D:P = 상속 끊기. 지금 사용자·SYSTEM(SY)·관리자(BA)에게 모든 권한(FA), 하위 폴더·파일에 상속(OICI)
        let sddl: Vec<u16> = format!("D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)").encode_utf16().chain(Some(0)).collect();
        let path: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            let mut sd: PSECURITY_DESCRIPTOR = null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut sd, null_mut()) == 0 {
                return false;
            }
            let (mut present, mut defaulted) = (0, 0);
            let mut dacl: *mut ACL = null_mut();
            let done = GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) != 0
                && present != 0
                && SetNamedSecurityInfoW(
                    path.as_ptr(),
                    SE_FILE_OBJECT,
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    null_mut(),
                    null_mut(),
                    dacl,
                    null_mut(),
                ) == 0;
            LocalFree(sd.cast());
            done
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    pub fn make_private(_dir: &std::path::Path) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_dir_stays_usable_by_owner() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("profiles");
        std::fs::create_dir(&p).unwrap();
        assert!(make_private(&p), "권한 설정 성공");
        // 본인은 그대로 읽고 쓸 수 있고, 안에 만든 것도 같은 권한을 물려받음
        std::fs::create_dir(p.join("claude-1")).unwrap();
        std::fs::write(p.join("claude-1").join("x.json"), "{}").unwrap();
        assert_eq!(std::fs::read_to_string(p.join("claude-1").join("x.json")).unwrap(), "{}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o700);
        }
    }
}
