#[cfg(windows)]
fn find_window() -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
    use windows::core::PCWSTR;
    let title: Vec<u16> = concat!("Seed Helper v", env!("CARGO_PKG_VERSION"), "\0")
        .encode_utf16()
        .collect();
    unsafe { FindWindowW(PCWSTR::null(), PCWSTR(title.as_ptr())).ok() }
}

#[cfg(windows)]
pub fn minimize_window() {
    use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_MINIMIZE};
    if let Some(hwnd) = find_window() {
        unsafe { let _ = ShowWindow(hwnd, SW_MINIMIZE); }
    }
}


#[cfg(windows)]
pub fn cursor_pos() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut pt = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut pt).ok().map(|_| (pt.x, pt.y)) }
}

#[cfg(not(windows))]
pub fn minimize_window() {}


#[cfg(not(windows))]
pub fn cursor_pos() -> Option<(i32, i32)> { None }

#[cfg(windows)]
pub fn primary_monitor_resolution() -> (u32, u32) {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    unsafe {
        (
            GetSystemMetrics(SM_CXSCREEN) as u32,
            GetSystemMetrics(SM_CYSCREEN) as u32,
        )
    }
}

#[cfg(not(windows))]
pub fn primary_monitor_resolution() -> (u32, u32) {
    (1920, 1080)
}
