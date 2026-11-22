//! 알림 영역(트레이) 아이콘 — 고정 GUID로 등록해 위치/표시 설정이 유지되게.
//!
//! - 실행 경로에서 만든 고정 GUID(NIF_GUID): 재실행해도 Windows가 같은 아이콘으로 인식
//! - 작업 표시줄 재생성(TaskbarCreated) 때 같은 ID로 다시 등록 (호스트 창이 알려 줌)
//! - 등록 실패 시: 남은 등록 지우고 재시도 → GUID 없이 등록 → 2초마다 재시도(로그인 직후 대비)

use crate::win::{self, copy_wide, Rgb};
use std::collections::{HashMap, VecDeque};
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub const WM_TRAY: u32 = WM_APP + 1;
const ICON_CACHE_MAX: usize = 600;

fn stable_guid() -> GUID {
    // Windows는 트레이 GUID를 실행 파일 경로에 묶으므로 경로마다 고정된 값을 만듦
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().to_lowercase()).unwrap_or_default();
    let mut a: u64 = 0xcbf2_9ce4_8422_2325;
    let mut b: u64 = 0x8422_2325_cbf2_9ce4;
    for byte in format!("usagi-tray://{exe}").bytes() {
        a = (a ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3);
        b = (b ^ byte as u64).wrapping_mul(0x0000_0100_0000_01b3).rotate_left(5);
    }
    let v = ((a as u128) << 64) | b as u128;
    // RFC 4122 형식 비트 (버전 5 자리)
    let v = (v & !(0xF000u128 << 64)) | (0x5000u128 << 64);
    GUID::from_u128(v)
}

pub struct Tray {
    hwnd: HWND,
    guid: GUID,
    use_guid: bool,
    added: bool,
    pub retry_pending: bool,
    tip: String,
    icons: HashMap<u64, HICON>,
    order: VecDeque<u64>,
    current: HICON,
    current_key: Option<u64>,
    balloon: HICON,
}

impl Tray {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            guid: stable_guid(),
            use_guid: true,
            added: false,
            retry_pending: false,
            tip: "usAgI".into(),
            icons: HashMap::new(),
            order: VecDeque::new(),
            current: std::ptr::null_mut(),
            current_key: None,
            balloon: std::ptr::null_mut(),
        }
    }

    fn nid(&self, flags: u32) -> NOTIFYICONDATAW {
        let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = self.hwnd;
        nid.uID = 1;
        nid.uFlags = flags;
        if self.use_guid {
            nid.uFlags |= NIF_GUID;
            nid.guidItem = self.guid;
        }
        nid
    }

    fn send(&self, op: u32, flags: u32) -> bool {
        let mut nid = self.nid(flags);
        if flags & NIF_ICON != 0 {
            nid.hIcon = self.current;
        }
        if flags & NIF_TIP != 0 {
            copy_wide(&mut nid.szTip, &self.tip);
        }
        if flags & NIF_MESSAGE != 0 {
            nid.uCallbackMessage = WM_TRAY;
        }
        unsafe { Shell_NotifyIconW(op, &nid) != 0 }
    }

    /// 등록. 실패하면 false (호스트가 2초 뒤 다시 부름)
    pub fn add(&mut self) -> bool {
        if self.current.is_null() {
            return false;
        }
        let flags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
        let mut ok = self.send(NIM_ADD, flags);
        if !ok && self.use_guid {
            // 이전 실행이 비정상 종료돼 같은 GUID가 남아 있으면 지우고 다시
            self.send(NIM_DELETE, 0);
            ok = self.send(NIM_ADD, flags);
        }
        if !ok && self.use_guid {
            // GUID가 다른 경로에 묶여 있으면 GUID 없이라도 표시
            self.use_guid = false;
            ok = self.send(NIM_ADD, flags);
        }
        if ok {
            let mut nid = self.nid(0);
            nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            unsafe { Shell_NotifyIconW(NIM_SETVERSION, &nid) };
        }
        self.added = ok;
        ok
    }

    /// 작업 표시줄이 새로 만들어졌을 때
    pub fn readd(&mut self) -> bool {
        self.added = false;
        self.add()
    }

    pub fn remove(&mut self) {
        if self.added {
            self.send(NIM_DELETE, 0);
            self.added = false;
        }
    }

    pub fn is_added(&self) -> bool {
        self.added
    }

    /// key가 같으면 캐시한 아이콘을 다시 씀. 실패하면 false.
    pub fn set_icon(&mut self, key: u64, render: impl FnOnce() -> (Vec<u8>, u32, Rgb)) -> bool {
        if self.current_key == Some(key) && !self.current.is_null() {
            return true;
        }
        let icon = match self.icons.get(&key) {
            Some(&h) => h,
            None => {
                let (mask, size, color) = render();
                let h = win::hicon_from_mask(&mask, size, color);
                if h.is_null() {
                    return false;
                }
                self.icons.insert(key, h);
                self.order.push_back(key);
                self.evict();
                h
            }
        };
        self.current = icon;
        self.current_key = Some(key);
        if self.added {
            if !self.send(NIM_MODIFY, NIF_ICON) {
                return self.readd();
            }
            true
        } else {
            self.add()
        }
    }

    fn evict(&mut self) {
        while self.order.len() > ICON_CACHE_MAX {
            let Some(old) = self.order.pop_front() else { break };
            if Some(old) == self.current_key {
                self.order.push_back(old);
                break;
            }
            if let Some(h) = self.icons.remove(&old) {
                unsafe { DestroyIcon(h) };
            }
        }
    }

    /// DPI/테마가 바뀌어 아이콘을 새로 그려야 할 때
    pub fn clear_cache(&mut self) {
        for (k, h) in self.icons.drain() {
            if Some(k) != self.current_key {
                unsafe { DestroyIcon(h) };
            }
        }
        self.order.clear();
        if let Some(k) = self.current_key.take() {
            // 현재 아이콘은 다음 set_icon 때까지 표시 중이므로 캐시로 되돌려 나중에 해제
            self.icons.insert(k ^ 0x5A5A_5A5A, self.current);
            self.order.push_back(k ^ 0x5A5A_5A5A);
        }
    }

    pub fn set_tip(&mut self, text: &str) {
        let text: String = if text.chars().count() > 127 { text.chars().take(126).collect::<String>() + "…" } else { text.into() };
        if text != self.tip {
            self.tip = text;
            if self.added {
                self.send(NIM_MODIFY, NIF_TIP | NIF_SHOWTIP);
            }
        }
    }

    pub fn set_balloon_icon(&mut self, mask: &[u8], size: u32, color: Rgb) {
        if !self.balloon.is_null() {
            unsafe { DestroyIcon(self.balloon) };
        }
        self.balloon = win::hicon_from_mask(mask, size, color);
    }

    pub fn notify(&self, title: &str, message: &str) {
        if !self.added {
            return;
        }
        let mut nid = self.nid(NIF_INFO);
        copy_wide(&mut nid.szInfoTitle, title);
        copy_wide(&mut nid.szInfo, message);
        nid.dwInfoFlags = NIIF_RESPECT_QUIET_TIME;
        if !self.balloon.is_null() {
            nid.dwInfoFlags |= NIIF_USER | NIIF_LARGE_ICON;
            nid.hBalloonIcon = self.balloon;
        }
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) };
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        self.remove();
        unsafe {
            for h in self.icons.values() {
                DestroyIcon(*h);
            }
            if !self.balloon.is_null() {
                DestroyIcon(self.balloon);
            }
        }
    }
}
