//! 이 사용자만 쓰는 폴더 만들기와 확인.
//!
//! usAgI가 계정마다 만드는 프로필 폴더에는 공식 CLI가 로그인 파일을 둡니다.
//! usAgI는 로그인 상태와 사용량 조회에 필요한 파일을 읽고, 세션의 쓰기와 갱신은 CLI가 관리합니다.
//! 상위 폴더의 권한을 물려받지 않게 해서, 다른 사용자·샌드박스 계정·앱 컨테이너가 그 파일을 읽지 못하게 합니다.
//! - Windows: 상속을 끊고 지금 사용자·SYSTEM·관리자만 허용 (하위 폴더·파일에 상속)
//! - 그 외: 권한 0700
//!
//! exe 옆에서 찾은 폴더(포터블 data)는 쓰기 전에 믿을 수 있는지 확인합니다 ([`is_trusted_dir`]):
//! 링크가 아닌 실제 폴더이고, 소유자가 지금 사용자이며, 다른 사용자가 파일을 넣거나 바꿀 수 없어야 합니다.
//! (Windows는 SYSTEM·관리자도 믿음: 이미 모든 파일을 바꿀 수 있는 계정)

use std::path::Path;

/// 폴더를 이 사용자 전용으로 바꿈. 실패하면 false 반환.
pub fn make_private(dir: &Path) -> bool {
    imp::make_private(dir)
}

/// 이미 이 사용자 전용인지 (다시 [`make_private`]할 필요 없음)
pub fn is_private(dir: &Path) -> bool {
    real(dir, true) && imp::private(dir)
}

/// 믿을 수 있는 폴더인지: 링크(심볼릭 링크·정션)가 아닌 실제 폴더, 소유자가 지금 사용자, 다른 사용자에게 쓰기 권한 없음
pub fn is_trusted_dir(dir: &Path) -> bool {
    real(dir, true) && imp::trusted(dir)
}

/// 믿을 수 있는 파일인지: 링크가 아닌 일반 파일, 조건은 [`is_trusted_dir`]와 같음
pub fn is_trusted_file(path: &Path) -> bool {
    real(path, false) && imp::trusted(path)
}

/// 링크를 따라가지 않고 본 실제 폴더(dir) 또는 일반 파일. Windows는 재분석 지점(정션 등)도 제외
fn real(path: &Path, dir: bool) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return false };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    if dir { meta.is_dir() } else { meta.is_file() }
}

#[cfg(unix)]
mod imp {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::Path;

    extern "C" {
        fn geteuid() -> u32;
    }

    fn owned(meta: &std::fs::Metadata) -> bool {
        // SAFETY: geteuid has no preconditions and cannot fail.
        meta.uid() == unsafe { geteuid() }
    }

    pub fn make_private(dir: &Path) -> bool {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).is_ok()
    }

    pub fn private(dir: &Path) -> bool {
        std::fs::symlink_metadata(dir).is_ok_and(|meta| owned(&meta) && meta.mode() & 0o077 == 0)
    }

    /// 그룹·다른 사용자 쓰기 권한(0o022)이 없어야 함
    pub fn trusted(path: &Path) -> bool {
        std::fs::symlink_metadata(path).is_ok_and(|meta| owned(&meta) && meta.mode() & 0o022 == 0)
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr::null_mut;
    use windows_sys::Win32::Foundation::{CloseHandle, LocalFree, HANDLE};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetNamedSecurityInfoW, SetNamedSecurityInfoW,
        SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{
        EqualSid, GetAce, GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetTokenInformation, IsWellKnownSid, TokenUser,
        WinBuiltinAdministratorsSid, WinLocalSystemSid, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION,
        OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// 지금 사용자 (TOKEN_USER + SID, 최대 68바이트. 포인터 정렬을 맞추려고 u64 배열)
    struct User([u64; 32]);

    impl User {
        fn current() -> Option<Self> {
            unsafe {
                let mut token: HANDLE = null_mut();
                if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                    return None;
                }
                let mut user = User([0; 32]);
                let mut len = 0u32;
                let ok = GetTokenInformation(token, TokenUser, user.0.as_mut_ptr().cast(), std::mem::size_of_val(&user.0) as u32, &mut len);
                CloseHandle(token);
                (ok != 0).then_some(user)
            }
        }

        fn sid(&self) -> PSID {
            unsafe { (*(self.0.as_ptr() as *const TOKEN_USER)).User.Sid }
        }

        /// SID 문자열 ("S-1-5-21-…")
        fn sid_string(&self) -> Option<String> {
            unsafe {
                let mut text: *mut u16 = null_mut();
                if ConvertSidToStringSidW(self.sid(), &mut text) == 0 || text.is_null() {
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

        /// 지금 사용자·SYSTEM·관리자 그룹
        fn trusts(&self, sid: PSID) -> bool {
            unsafe {
                !sid.is_null()
                    && (EqualSid(sid, self.sid()) != 0
                        || IsWellKnownSid(sid, WinLocalSystemSid) != 0
                        || IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0)
            }
        }
    }

    /// 파일·폴더의 소유자와 DACL (보안 설명자는 LocalFree로 해제)
    struct Security {
        descriptor: PSECURITY_DESCRIPTOR,
        owner: PSID,
        dacl: *mut ACL,
    }

    impl Security {
        fn read(path: &Path) -> Option<Self> {
            let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
            let mut security = Security { descriptor: null_mut(), owner: null_mut(), dacl: null_mut() };
            let status = unsafe {
                GetNamedSecurityInfoW(
                    path.as_ptr(),
                    SE_FILE_OBJECT,
                    OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                    &mut security.owner,
                    null_mut(),
                    &mut security.dacl,
                    null_mut(),
                    &mut security.descriptor,
                )
            };
            (status == 0).then_some(security)
        }

        fn protected(&self) -> bool {
            let (mut control, mut revision) = (0u16, 0u32);
            unsafe { GetSecurityDescriptorControl(self.descriptor, &mut control, &mut revision) != 0 && control & 0x1000 != 0 }
        }

        /// 이 개체에 적용되는 허용 ACE마다 (권한, SID). 해석할 수 없는 허용 ACE는 (모든 권한, null SID)
        fn allowed(&self) -> Vec<(u32, PSID)> {
            const INHERIT_ONLY: u8 = 0x08;
            let mut out = Vec::new();
            unsafe {
                for index in 0..u32::from((*self.dacl).AceCount) {
                    let mut ace: *mut core::ffi::c_void = null_mut();
                    if GetAce(self.dacl, index, &mut ace) == 0 {
                        out.push((u32::MAX, null_mut()));
                        continue;
                    }
                    let header = &*(ace as *const ACE_HEADER);
                    if header.AceFlags & INHERIT_ONLY != 0 {
                        continue; // 하위 항목에만 상속되는 ACE
                    }
                    match header.AceType {
                        // ACCESS_ALLOWED_ACE_TYPE, ACCESS_ALLOWED_CALLBACK_ACE_TYPE: 같은 구조
                        0 | 9 => {
                            let allowed = &*(ace as *const ACCESS_ALLOWED_ACE);
                            out.push((allowed.Mask, &allowed.SidStart as *const u32 as PSID));
                        }
                        // ACCESS_ALLOWED_COMPOUND·OBJECT·CALLBACK_OBJECT: 파일에는 쓰지 않는 형식이라 믿지 않음
                        4 | 5 | 11 => out.push((u32::MAX, null_mut())),
                        _ => {} // 거부·감사 ACE
                    }
                }
            }
            out
        }
    }

    impl Drop for Security {
        fn drop(&mut self) {
            unsafe { LocalFree(self.descriptor.cast()) };
        }
    }

    /// 파일을 넣거나 지우거나 바꿀 수 있는 권한: 쓰기·추가(폴더: 파일·하위 폴더 만들기), 하위 항목 삭제, 삭제, 권한·소유자 변경, GENERIC_WRITE·ALL
    const WRITE_RIGHTS: u32 = 0x0002 | 0x0004 | 0x0040 | 0x0001_0000 | 0x0004_0000 | 0x0008_0000 | 0x4000_0000 | 0x1000_0000;

    pub fn trusted(path: &Path) -> bool {
        let (Some(user), Some(security)) = (User::current(), Security::read(path)) else { return false };
        // DACL이 없으면(FAT·exFAT 등) 누구나 쓸 수 있음
        user.trusts(security.owner)
            && !security.dacl.is_null()
            && security.allowed().into_iter().all(|(mask, sid)| mask & WRITE_RIGHTS == 0 || user.trusts(sid))
    }

    pub fn private(dir: &Path) -> bool {
        let (Some(user), Some(security)) = (User::current(), Security::read(dir)) else { return false };
        user.trusts(security.owner)
            && !security.dacl.is_null()
            && security.protected()
            && security.allowed().into_iter().all(|(_, sid)| user.trusts(sid))
    }

    pub fn make_private(dir: &Path) -> bool {
        let Some(sid) = User::current().and_then(|user| user.sid_string()) else { return false };
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

    /// 테스트용: SDDL로 DACL을 바꿈 (예: 모든 사용자 쓰기 허용)
    #[cfg(test)]
    pub fn set_dacl(path: &Path, sddl: &str) -> bool {
        let sddl: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        unsafe {
            let mut sd: PSECURITY_DESCRIPTOR = null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), 1, &mut sd, null_mut()) == 0 {
                return false;
            }
            let (mut present, mut defaulted) = (0, 0);
            let mut dacl: *mut ACL = null_mut();
            let done = GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) != 0
                && SetNamedSecurityInfoW(path.as_ptr(), SE_FILE_OBJECT, DACL_SECURITY_INFORMATION, null_mut(), null_mut(), dacl, null_mut()) == 0;
            LocalFree(sd.cast());
            done
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::path::Path;

    pub fn make_private(_dir: &Path) -> bool {
        false
    }

    pub fn private(_dir: &Path) -> bool {
        false
    }

    pub fn trusted(_path: &Path) -> bool {
        false
    }
}

/// 테스트용: 다른 사용자도 쓸 수 있게 권한을 엶 (Unix 0o777, Windows 모든 사용자 모든 권한)
#[cfg(test)]
pub(crate) fn open_to_everyone(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o777)).unwrap();
    }
    #[cfg(windows)]
    assert!(imp::set_dacl(path, if path.is_dir() { "D:(A;OICI;FA;;;WD)" } else { "D:(A;;FA;;;WD)" }));
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
        assert!(is_private(&p));
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

    #[test]
    fn trusts_only_real_folders_that_others_cannot_write() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        std::fs::create_dir(&data).unwrap();
        std::fs::write(data.join("state.json"), "{}").unwrap();
        assert!(is_trusted_dir(&data), "이 사용자가 만든 폴더");
        assert!(is_trusted_file(&data.join("state.json")));
        assert!(!is_trusted_dir(&data.join("state.json")), "파일은 폴더가 아님");
        assert!(!is_trusted_file(&data), "폴더는 파일이 아님");
        assert!(!is_trusted_dir(&root.path().join("missing")));

        open_to_everyone(&data);
        assert!(!is_trusted_dir(&data), "다른 사용자가 쓸 수 있음");
        assert!(!is_private(&data));
        assert!(make_private(&data));
        assert!(is_trusted_dir(&data) && is_private(&data), "이 사용자 전용으로 바꾼 뒤");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_folders_are_not_trusted() {
        let root = tempfile::tempdir().unwrap();
        let real = root.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, root.path().join("link")).unwrap();
        assert!(is_trusted_dir(&real));
        assert!(!is_trusted_dir(&root.path().join("link")));
        assert!(!is_private(&root.path().join("link")));
    }
}
