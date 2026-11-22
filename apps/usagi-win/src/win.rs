//! Win32 도우미: 문자열, 레지스트리, 테마/DPI, 비트맵(프리멀티 BGRA), 아이콘, GDI 글자.

use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

pub type Rgb = (u8, u8, u8);

pub const INK_ON_DARK: Rgb = (255, 255, 255);
pub const INK_ON_LIGHT: Rgb = (26, 26, 26);

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn copy_wide(dst: &mut [u16], s: &str) {
    let mut n = 0;
    for (i, c) in s.encode_utf16().take(dst.len().saturating_sub(1)).enumerate() {
        dst[i] = c;
        n = i + 1;
    }
    dst[n] = 0;
}

// ------------------------------------------------------------------ 레지스트리

pub fn reg_dword(subkey: &str, value: &str) -> Option<u32> {
    let mut data: u32 = 0;
    let mut size = 4u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(subkey).as_ptr(),
            wide(value).as_ptr(),
            RRF_RT_REG_DWORD,
            null_mut(),
            &mut data as *mut u32 as *mut c_void,
            &mut size,
        )
    };
    (r == ERROR_SUCCESS).then_some(data)
}

pub fn reg_string(subkey: &str, value: &str) -> Option<String> {
    let mut buf = vec![0u16; 2048];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            wide(subkey).as_ptr(),
            wide(value).as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buf.as_mut_ptr() as *mut c_void,
            &mut size,
        )
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// HKLM 문자열 값 (읽기 전용: Windows 빌드 번호 등)
pub fn reg_string_lm(subkey: &str, value: &str) -> Option<String> {
    let mut buf = vec![0u16; 256];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            wide(subkey).as_ptr(),
            wide(value).as_ptr(),
            RRF_RT_REG_SZ,
            null_mut(),
            buf.as_mut_ptr() as *mut c_void,
            &mut size,
        )
    };
    if r != ERROR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

pub fn reg_set_string(subkey: &str, value: &str, data: &str) -> bool {
    let w = wide(data);
    unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            wide(subkey).as_ptr(),
            wide(value).as_ptr(),
            REG_SZ,
            w.as_ptr() as *const c_void,
            (w.len() * 2) as u32,
        ) == ERROR_SUCCESS
    }
}

pub fn reg_delete(subkey: &str, value: &str) {
    unsafe {
        RegDeleteKeyValueW(HKEY_CURRENT_USER, wide(subkey).as_ptr(), wide(value).as_ptr());
    }
}

const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

/// 작업 표시줄(Windows 모드)이 밝은지
pub fn taskbar_is_light() -> bool {
    reg_dword(PERSONALIZE, "SystemUsesLightTheme") == Some(1)
}

/// 앱 모드가 밝은지 (설정 창)
pub fn apps_are_light() -> bool {
    reg_dword(PERSONALIZE, "AppsUseLightTheme") != Some(0)
}

pub fn theme_ink() -> Rgb {
    if taskbar_is_light() {
        INK_ON_LIGHT
    } else {
        INK_ON_DARK
    }
}

// ------------------------------------------------------------------ DPI

pub fn enable_dpi_awareness() {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

pub fn taskbar() -> HWND {
    unsafe { FindWindowW(wide("Shell_TrayWnd").as_ptr(), null()) }
}

pub fn taskbar_dpi() -> u32 {
    unsafe {
        let tb = taskbar();
        let d = if tb.is_null() { 0 } else { GetDpiForWindow(tb) };
        if d == 0 {
            GetDpiForSystem()
        } else {
            d
        }
    }
}

/// 작업 표시줄 작은 아이콘 크기: 100%=16, 150%=24, 200%=32
pub fn small_icon_size() -> u32 {
    let s = unsafe { GetSystemMetricsForDpi(SM_CXSMICON, taskbar_dpi()) };
    if s > 0 {
        s as u32
    } else {
        16
    }
}

pub fn window_rect(hwnd: HWND) -> Option<RECT> {
    if hwnd.is_null() {
        return None;
    }
    let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
    (unsafe { GetWindowRect(hwnd, &mut r) } != 0).then_some(r)
}

pub fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

// ------------------------------------------------------------------ 이미지

/// 프리멀티플라이드 BGRA 이미지
#[derive(Clone)]
pub struct Image {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

impl Image {
    pub fn new(w: u32, h: u32) -> Self {
        Self { w, h, px: vec![0; (w * h * 4) as usize] }
    }

    /// 단색 + 알파로 전체 채우기
    pub fn fill(&mut self, (r, g, b): Rgb, a: u8) {
        let m = |c: u8| ((c as u32 * a as u32 + 127) / 255) as u8;
        for p in self.px.chunks_exact_mut(4) {
            p.copy_from_slice(&[m(b), m(g), m(r), a]);
        }
    }

    /// 알파 마스크를 색으로 칠해 src-over 합성
    pub fn blend_mask(&mut self, x: i32, y: i32, mask: &[u8], mw: u32, mh: u32, (r, g, b): Rgb, opacity: u8) {
        for my in 0..mh as i32 {
            let ty = y + my;
            if ty < 0 || ty >= self.h as i32 {
                continue;
            }
            for mx in 0..mw as i32 {
                let tx = x + mx;
                if tx < 0 || tx >= self.w as i32 {
                    continue;
                }
                let cov = mask[(my as u32 * mw + mx as u32) as usize] as u32 * opacity as u32 / 255;
                if cov == 0 {
                    continue;
                }
                let i = ((ty as u32 * self.w + tx as u32) * 4) as usize;
                let inv = 255 - cov;
                let d = &mut self.px[i..i + 4];
                d[0] = ((b as u32 * cov + d[0] as u32 * inv + 127) / 255) as u8;
                d[1] = ((g as u32 * cov + d[1] as u32 * inv + 127) / 255) as u8;
                d[2] = ((r as u32 * cov + d[2] as u32 * inv + 127) / 255) as u8;
                d[3] = ((255 * cov + d[3] as u32 * inv + 127) / 255) as u8;
            }
        }
    }
}

/// 알파 마스크 + 색 → HICON (직선 알파 BGRA)
pub fn hicon_from_mask(mask: &[u8], size: u32, (r, g, b): Rgb) -> HICON {
    let mut bgra = Vec::with_capacity((size * size * 4) as usize);
    for &a in mask {
        bgra.extend_from_slice(&[b, g, r, a]);
    }
    unsafe {
        let hbm = dib_with(size, size, &bgra);
        if hbm.is_null() {
            return null_mut();
        }
        let mask_bm = CreateBitmap(size as i32, size as i32, 1, 1, null());
        let info = ICONINFO { fIcon: 1, xHotspot: 0, yHotspot: 0, hbmMask: mask_bm, hbmColor: hbm };
        let icon = CreateIconIndirect(&info);
        DeleteObject(hbm as HGDIOBJ);
        DeleteObject(mask_bm as HGDIOBJ);
        icon
    }
}

fn bitmap_info(w: u32, h: u32) -> BITMAPINFO {
    let mut bmi: BITMAPINFO = unsafe { std::mem::zeroed() };
    bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = w as i32;
    bmi.bmiHeader.biHeight = -(h as i32); // top-down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB;
    bmi
}

/// 32bpp DIB 섹션을 만들고 픽셀을 복사
unsafe fn dib_with(w: u32, h: u32, bgra: &[u8]) -> HBITMAP {
    let bmi = bitmap_info(w, h);
    let screen = GetDC(null_mut());
    let mut bits: *mut c_void = null_mut();
    let hbm = CreateDIBSection(screen, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    ReleaseDC(null_mut(), screen);
    if !hbm.is_null() && !bits.is_null() {
        std::ptr::copy_nonoverlapping(bgra.as_ptr(), bits as *mut u8, bgra.len().min((w * h * 4) as usize));
    }
    hbm
}

/// 레이어드 창에 프리멀티 이미지를 그림 (per-pixel alpha)
pub fn blit_layered(hwnd: HWND, x: i32, y: i32, img: &Image) {
    unsafe {
        let screen = GetDC(null_mut());
        let mem = CreateCompatibleDC(screen);
        let hbm = dib_with(img.w, img.h, &img.px);
        let old = SelectObject(mem, hbm as HGDIOBJ);
        let dst = POINT { x, y };
        let src = POINT { x: 0, y: 0 };
        let size = SIZE { cx: img.w as i32, cy: img.h as i32 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        UpdateLayeredWindow(hwnd, screen, &dst, &size, mem, &src, 0, &blend, ULW_ALPHA);
        SelectObject(mem, old);
        DeleteObject(hbm as HGDIOBJ);
        DeleteDC(mem);
        ReleaseDC(null_mut(), screen);
    }
}

// ------------------------------------------------------------------ 글자 (GDI)

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontSpec {
    pub px: i32,
    pub weight: i32,
    pub face: &'static str,
}

impl FontSpec {
    /// 숫자: Segoe UI Semibold (힌팅이 좋아 작은 크기에서도 또렷)
    pub fn value(px: i32) -> Self {
        Self { px, weight: 600, face: "Segoe UI" }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// GDI로 글자를 흰색 그레이스케일 안티앨리어싱으로 그려 커버리지(알파)로 씁니다.
/// 투명 창 위에서도 힌팅된 또렷한 글자가 나옵니다 (ClearType은 투명 배경에서 불가).
pub struct TextRenderer {
    dc: HDC,
    fonts: HashMap<FontSpec, HFONT>,
}

impl TextRenderer {
    pub fn new() -> Self {
        let dc = unsafe { CreateCompatibleDC(null_mut()) };
        Self { dc, fonts: HashMap::new() }
    }

    fn font(&mut self, spec: FontSpec) -> HFONT {
        *self.fonts.entry(spec).or_insert_with(|| unsafe {
            CreateFontW(
                -spec.px,
                0,
                0,
                0,
                spec.weight,
                0,
                0,
                0,
                DEFAULT_CHARSET as _,
                OUT_TT_PRECIS as _,
                CLIP_DEFAULT_PRECIS as _,
                ANTIALIASED_QUALITY as _,
                (DEFAULT_PITCH as u32 | FF_DONTCARE as u32) as _,
                wide(spec.face).as_ptr(),
            )
        })
    }

    pub fn measure(&mut self, text: &str, spec: FontSpec) -> (i32, i32) {
        let font = self.font(spec);
        let w: Vec<u16> = text.encode_utf16().collect();
        let mut size = SIZE { cx: 0, cy: 0 };
        unsafe {
            let old = SelectObject(self.dc, font as HGDIOBJ);
            GetTextExtentPoint32W(self.dc, w.as_ptr(), w.len() as i32, &mut size);
            SelectObject(self.dc, old);
        }
        (size.cx, size.cy)
    }

    /// y_center를 세로 중심으로, align에 따라 x를 왼쪽/오른쪽 기준으로 그림
    pub fn draw(&mut self, img: &mut Image, x: i32, y_center: i32, text: &str, spec: FontSpec, color: Rgb, opacity: u8, align: Align) -> i32 {
        if text.is_empty() {
            return 0;
        }
        let (tw, th) = self.measure(text, spec);
        let (w, h) = ((tw + 4).max(1) as u32, (th + 2).max(1) as u32);
        let font = self.font(spec);
        let wtext: Vec<u16> = text.encode_utf16().collect();
        let mut mask = vec![0u8; (w * h) as usize];
        unsafe {
            let bmi = bitmap_info(w, h);
            let mut bits: *mut c_void = null_mut();
            let hbm = CreateDIBSection(self.dc, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if hbm.is_null() || bits.is_null() {
                return tw;
            }
            let old_bm = SelectObject(self.dc, hbm as HGDIOBJ);
            let old_font = SelectObject(self.dc, font as HGDIOBJ);
            SetBkMode(self.dc, TRANSPARENT as _);
            SetTextColor(self.dc, 0x00FF_FFFF);
            TextOutW(self.dc, 1, 1, wtext.as_ptr(), wtext.len() as i32);
            GdiFlush();
            let px = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize);
            for (i, m) in mask.iter_mut().enumerate() {
                let p = &px[i * 4..i * 4 + 3];
                *m = p[0].max(p[1]).max(p[2]);
            }
            SelectObject(self.dc, old_font);
            SelectObject(self.dc, old_bm);
            DeleteObject(hbm as HGDIOBJ);
        }
        let left = match align {
            Align::Left => x - 1,
            Align::Right => x - tw - 1,
        };
        img.blend_mask(left, y_center - th / 2 - 1, &mask, w, h, color, opacity);
        tw
    }
}

impl Drop for TextRenderer {
    fn drop(&mut self) {
        unsafe {
            for f in self.fonts.values() {
                DeleteObject(*f as HGDIOBJ);
            }
            DeleteDC(self.dc);
        }
    }
}

// ------------------------------------------------------------------ 모니터 / 작업 표시줄

#[derive(Clone)]
pub struct Monitor {
    /// \.\DISPLAY2 같은 장치 이름 (설정에 저장하는 값)
    pub device: String,
    /// 표시용 이름 (모니터 제품명)
    pub name: String,
    pub rect: RECT,
    pub primary: bool,
}

fn from_wide(buf: &[u16]) -> String {
    let n = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..n])
}

fn monitor_info(hmon: HMONITOR) -> Option<(String, RECT, bool)> {
    let mut mi: MONITORINFOEXW = unsafe { std::mem::zeroed() };
    mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if unsafe { GetMonitorInfoW(hmon, &mut mi as *mut MONITORINFOEXW as *mut MONITORINFO) } == 0 {
        return None;
    }
    Some((from_wide(&mi.szDevice), mi.monitorInfo.rcMonitor, mi.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0))
}

fn friendly_name(device: &str) -> String {
    let mut dd: DISPLAY_DEVICEW = unsafe { std::mem::zeroed() };
    dd.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
    if unsafe { EnumDisplayDevicesW(wide(device).as_ptr(), 0, &mut dd, 0) } != 0 {
        let s = from_wide(&dd.DeviceString);
        if !s.trim().is_empty() {
            return s;
        }
    }
    device.trim_start_matches(r"\.\").to_string()
}

unsafe extern "system" fn collect_monitor(hmon: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let list = &mut *(data as *mut Vec<HMONITOR>);
    list.push(hmon);
    1
}

/// 장치 이름(\.\DISPLAY1) → 모니터 제품명 (EDID). 드라이버 이름("Generic PnP Monitor")이 아닌 실제 이름.
fn edid_names() -> HashMap<String, String> {
    use windows_sys::Win32::Devices::Display::*;
    let mut map = HashMap::new();
    unsafe {
        let (mut np, mut nm) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut np, &mut nm) != ERROR_SUCCESS {
            return map;
        }
        let mut paths = vec![std::mem::zeroed::<DISPLAYCONFIG_PATH_INFO>(); np as usize];
        let mut modes = vec![std::mem::zeroed::<DISPLAYCONFIG_MODE_INFO>(); nm as usize];
        if QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut np, paths.as_mut_ptr(), &mut nm, modes.as_mut_ptr(), null_mut()) != ERROR_SUCCESS {
            return map;
        }
        for p in &paths[..np as usize] {
            let mut src: DISPLAYCONFIG_SOURCE_DEVICE_NAME = std::mem::zeroed();
            src.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            src.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            src.header.adapterId = p.sourceInfo.adapterId;
            src.header.id = p.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut src.header) != 0 {
                continue;
            }
            let mut tgt: DISPLAYCONFIG_TARGET_DEVICE_NAME = std::mem::zeroed();
            tgt.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            tgt.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            tgt.header.adapterId = p.targetInfo.adapterId;
            tgt.header.id = p.targetInfo.id;
            if DisplayConfigGetDeviceInfo(&mut tgt.header) != 0 {
                continue;
            }
            let mut name = from_wide(&tgt.monitorFriendlyDeviceName);
            if name.trim().is_empty() {
                let internal = tgt.outputTechnology == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL
                    || tgt.outputTechnology == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED
                    || tgt.outputTechnology == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED;
                if internal {
                    name = "내장 디스플레이".into();
                }
            }
            if !name.trim().is_empty() {
                map.insert(from_wide(&src.viewGdiDeviceName).to_lowercase(), name.trim().to_string());
            }
        }
    }
    map
}

pub fn monitors() -> Vec<Monitor> {
    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        EnumDisplayMonitors(null_mut(), null(), Some(collect_monitor), &mut handles as *mut Vec<HMONITOR> as LPARAM);
    }
    let edid = edid_names();
    let mut out: Vec<Monitor> = handles
        .into_iter()
        .filter_map(monitor_info)
        .map(|(device, rect, primary)| {
            let name = edid.get(&device.to_lowercase()).cloned().unwrap_or_else(|| friendly_name(&device));
            Monitor { name, device, rect, primary }
        })
        .collect();
    // 주 모니터 먼저, 그다음 왼쪽→오른쪽
    out.sort_by_key(|m| (!m.primary, m.rect.left, m.rect.top));
    out
}

pub fn monitor_device_of(hwnd: HWND) -> Option<(String, bool)> {
    let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    monitor_info(hmon).map(|(d, _, p)| (d, p))
}

#[derive(Clone, Debug)]
pub struct Taskbar {
    pub hwnd: HWND,
    pub device: String,
    pub primary: bool,
}

/// 모든 작업 표시줄 (주 작업 표시줄 먼저)
pub fn taskbars() -> Vec<Taskbar> {
    let mut out = Vec::new();
    let main = taskbar();
    if !main.is_null() {
        let (device, _) = monitor_device_of(main).unwrap_or_default();
        out.push(Taskbar { hwnd: main, device, primary: true });
    }
    let class = wide("Shell_SecondaryTrayWnd");
    let mut prev: HWND = null_mut();
    loop {
        let h = unsafe { FindWindowExW(null_mut(), prev, class.as_ptr(), null()) };
        if h.is_null() {
            break;
        }
        if let Some((device, _)) = monitor_device_of(h) {
            out.push(Taskbar { hwnd: h, device, primary: false });
        }
        prev = h;
    }
    out
}
