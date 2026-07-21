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
    use windows::Win32::Foundation::{HWND, LPARAM, BOOL};

    const SCANCODE_CONSOLE: u16 = 0x29; // ` / ё  — layout-independent console key
    const SCANCODE_ENTER: u16   = 0x1C;
    const SCANCODE_ALT: u16     = 0x38;

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

    // Find Squad window
    let mut target: HWND = HWND(std::ptr::null_mut());
    unsafe extern "system" fn enum_cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let target = &mut *(lparam.0 as *mut HWND);
        let mut buf = [0u16; 256];
        let len = GetWindowTextW(hwnd, &mut buf);
        let title = String::from_utf16_lossy(&buf[..len as usize]);
        if (title.contains("SquadGame") || title == "Squad") && IsWindowVisible(hwnd).as_bool() {
            *target = hwnd;
            return BOOL(0); // stop enumeration
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
        }

        // Alt press trick to allow SetForegroundWindow
        send_scan(SCANCODE_ALT, false);
        send_scan(SCANCODE_ALT, true);
        let _ = SetForegroundWindow(target);
        std::thread::sleep(std::time::Duration::from_millis(500));

        if GetForegroundWindow() != target {
            anyhow::bail!("Не удалось получить фокус окна Squad");
        }

        // Open console
        send_scan(SCANCODE_CONSOLE, false);
        std::thread::sleep(std::time::Duration::from_millis(50));
        send_scan(SCANCODE_CONSOLE, true);
        std::thread::sleep(std::time::Duration::from_millis(1000));

        // Type command (Unicode — layout-independent)
        for ch in "createsquad 12 0".chars() {
            send_unicode(ch);
        }
        std::thread::sleep(std::time::Duration::from_millis(500));

        // Confirm
        send_scan(SCANCODE_ENTER, false);
        std::thread::sleep(std::time::Duration::from_millis(50));
        send_scan(SCANCODE_ENTER, true);
    }

    Ok(())
}
