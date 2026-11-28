#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() {
    // 실행 중에 읽는 DLL도 System32에서만 (exe가 있는 임시 폴더·작업 폴더의 같은 이름 DLL을 쓰지 않게)
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::LibraryLoader::{SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_SYSTEM32};
        SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32);
    }
    usagi_settings::main_window();
}
