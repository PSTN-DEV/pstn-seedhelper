//! Win32 keyboard injection for in-game squad creation.
//! All functions are Windows-only; stubs on other platforms.

use tokio_util::sync::CancellationToken;
use crate::app::LogSender;

pub async fn create_ingame_squad(token: &CancellationToken, log: &LogSender) {
    let _ = log.send("Подключение успешно! Ждём 10 сек перед созданием сквада...".into());

    tokio::select! {
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(10)) => {}
        _ = token.cancelled() => return,
    }

    #[cfg(windows)]
    {
        if let Err(e) = windows_create_squad() {
            let _ = log.send(format!("Ошибка создания сквада: {e}"));
        } else {
            let _ = log.send("Сквад создан".into());
        }
    }
    #[cfg(not(windows))]
    {
        let _ = log.send("Автосоздание сквада поддерживается только на Windows".into());
    }
}

#[cfg(windows)]
fn windows_create_squad() -> anyhow::Result<()> {
    use std::mem;
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM, BOOL};

    // Console key: sent via PostMessage(WM_KEYDOWN/WM_CHAR/WM_KEYUP) with explicit
    // VK_OEM_3 (0xC0) and the '`' character — fully layout-independent.
    // SendInput uses the window's active layout to translate scancodes, which would
    // produce ё on Russian/Ukrainian layouts; PostMessage bypasses that entirely.
    // Command text uses KEYEVENTF_UNICODE — also layout-independent.
    const SCANCODE_ENTER: u16 = 0x1C;

    unsafe fn send_scan(scan: u16, key_up: bool) {
        let flags = KEYEVENTF_SCANCODE | if key_up { KEYEVENTF_KEYUP } else { KEYBD_EVENT_FLAGS(0) };
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(0),
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        SendInput(&[input], mem::size_of::<INPUT>() as i32);
    }

    unsafe fn send_unicode(ch: char) {
        for &flags in &[KEYEVENTF_UNICODE, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP] {
            let input = INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VIRTUAL_KEY(0),
                        wScan: ch as u16,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            };
            SendInput(&[input], mem::size_of::<INPUT>() as i32);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    // Find Squad window by title
    let mut target: HWND = HWND(std::ptr::null_mut());
    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let target = &mut *(lparam.0 as *mut HWND);
        let mut buf = [0u16; 256];
        let len = GetWindowTextW(hwnd, &mut buf);
        let title = String::from_utf16_lossy(&buf[..len as usize]);
        if (title.contains("SquadGame") || title == "Squad") && IsWindowVisible(hwnd).as_bool() {
            *target = hwnd;
            return BOOL(0);
        }
        BOOL(1)
    }

    unsafe {
        EnumWindows(Some(enum_cb), LPARAM(&mut target as *mut HWND as isize))?;
    }

    if target.0.is_null() {
        anyhow::bail!("Окно Squad не найдено");
    }

    unsafe {
        // Restore if minimized
        if IsIconic(target).as_bool() {
            let _ = ShowWindow(target, SW_RESTORE);
            std::thread::sleep(std::time::Duration::from_millis(300));
        }

        // AttachThreadInput is the reliable Win32 way to steal foreground focus
        // from a background process without the normal UIPI restriction.
        let squad_tid = GetWindowThreadProcessId(target, None);
        let our_tid   = GetCurrentThreadId();
        let _ = AttachThreadInput(our_tid, squad_tid, BOOL(1));
        let _ = BringWindowToTop(target);
        let _ = SetForegroundWindow(target);
        let _ = AttachThreadInput(our_tid, squad_tid, BOOL(0));

        std::thread::sleep(std::time::Duration::from_millis(300));

        // Open console via PostMessage — bypasses keyboard layout translation.
        // SendInput with scancode 0x29 produces ё on Russian/Ukrainian layouts;
        // PostMessage with explicit VK_OEM_3 (0xC0) + '`' char is always correct.
        let lp_dn = LPARAM(1_isize | (0x29_isize << 16));
        let lp_up = LPARAM(1_isize | (0x29_isize << 16) | (0xC000_0000_u32 as i32 as isize));
        let _ = PostMessageW(target, WM_KEYDOWN, WPARAM(0xC0), lp_dn);
        std::thread::sleep(std::time::Duration::from_millis(30));
        let _ = PostMessageW(target, WM_CHAR, WPARAM(0x60), lp_dn); // 0x60 = '`'
        std::thread::sleep(std::time::Duration::from_millis(30));
        let _ = PostMessageW(target, WM_KEYUP, WPARAM(0xC0), lp_up);
        std::thread::sleep(std::time::Duration::from_millis(600));

        // Type command — KEYEVENTF_UNICODE bypasses keyboard layout entirely
        for ch in "createsquad 12 0".chars() {
            send_unicode(ch);
        }
        std::thread::sleep(std::time::Duration::from_millis(300));

        // Confirm
        send_scan(SCANCODE_ENTER, false);
        std::thread::sleep(std::time::Duration::from_millis(50));
        send_scan(SCANCODE_ENTER, true);
    }

    Ok(())
}
