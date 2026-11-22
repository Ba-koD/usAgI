//! Windows message-loop adapter for the shared, on-demand settings process.
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

pub use usagi_settings::SettingsWindow;
pub const WM_IPC: u32 = WM_APP + 11;

pub fn open(host: HWND) -> std::io::Result<SettingsWindow> {
    let host = host as isize;
    SettingsWindow::open(move || unsafe {
        PostMessageW(host as HWND, WM_IPC, 0, 0);
    })
}
