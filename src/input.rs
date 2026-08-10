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
    use std::mem::size_of;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, VIRTUAL_KEY,
        KEYEVENTF_KEYUP, LoadKeyboardLayoutW, GetKeyboardLayout,
        KLF_ACTIVATE,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM, BOOL};

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
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut target as *mut HWND as isize));
    }

    if target.0.is_null() {
        anyhow::bail!("Окно Squad не найдено");
    }

    // Sends a virtual-key INPUT event, tagged with guard::MAGIC in dwExtraInfo so the
    // low-level keyboard hook lets OUR keys through while swallowing the user's. wVk is
    // passed through to WM_KEYDOWN verbatim (no layout translation without SCANCODE), which
    // lets us force VK_OEM_3 regardless of the active keyboard layout.
    let vk_event = |vk: u16, scan: u16, up: bool| -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: scan,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(0) },
                    time: 0,
                    dwExtraInfo: crate::guard::MAGIC,
                },
            },
        }
    };

    unsafe {
        let squad_tid = GetWindowThreadProcessId(target, None);
        let our_tid   = GetCurrentThreadId();
        #[link(name = "user32")]
        extern "system" {
            fn SetFocus(hwnd: HWND) -> HWND;
            fn SetActiveWindow(hwnd: HWND) -> HWND;
            fn SystemParametersInfoW(action: u32, param: u32, pvparam: *mut core::ffi::c_void, ini: u32) -> BOOL;
        }

        use windows::Win32::UI::WindowsAndMessaging::{WA_ACTIVE, GetForegroundWindow};

        // Start the guard: low-level hooks now swallow the user's keys/clicks (only our
        // MAGIC-tagged keys pass) and a red overlay covers every monitor. F12 sets abort.
        let overlays = crate::guard::begin();

        const SPI_GETFOREGROUNDLOCKTIMEOUT: u32 = 0x2000;
        const SPI_SETFOREGROUNDLOCKTIMEOUT: u32 = 0x2001;
        let mut old_timeout: u32 = 200;
        let _ = SystemParametersInfoW(SPI_GETFOREGROUNDLOCKTIMEOUT, 0, &mut old_timeout as *mut u32 as *mut _, 0);
        let _ = SystemParametersInfoW(SPI_SETFOREGROUNDLOCKTIMEOUT, 0, std::ptr::null_mut(), 0);
        let _ = AttachThreadInput(our_tid, squad_tid, BOOL(1));

        // Raise Squad and CONFIRM it is foreground before typing (never type blind). We
        // attach to the *foreground* thread (the blocker) to share its input credential and
        // tap Alt so we become the last to inject input — the two tricks that actually win
        // the steal. The hook already blocks the user, so we always run "forced".
        let attempt = |tries: i32| -> bool {
            for _ in 0..tries {
                if crate::guard::aborted() {
                    return false;
                }
                let fg_tid = {
                    let fg = GetForegroundWindow();
                    if fg.0.is_null() { 0 } else { GetWindowThreadProcessId(fg, None) }
                };
                let borrow = fg_tid != 0 && fg_tid != our_tid && fg_tid != squad_tid;
                if borrow {
                    let _ = AttachThreadInput(our_tid, fg_tid, BOOL(1));
                }
                let _ = SendInput(&[vk_event(0x12, 0x38, false), vk_event(0x12, 0x38, true)],
                                  size_of::<INPUT>() as i32);
                if IsIconic(target).as_bool() {
                    let _ = ShowWindow(target, SW_RESTORE);
                }
                let _ = BringWindowToTop(target);
                let _ = SetForegroundWindow(target);
                SetActiveWindow(target);
                SetFocus(target);
                let _ = PostMessageW(target, WM_ACTIVATE, WPARAM(WA_ACTIVE as usize), LPARAM(0));
                let _ = PostMessageW(target, WM_SETFOCUS, WPARAM(0), LPARAM(0));
                if borrow {
                    let _ = AttachThreadInput(our_tid, fg_tid, BOOL(0));
                }
                crate::guard::pump(80);
                if GetForegroundWindow().0 == target.0 {
                    return true;
                }
            }
            false
        };

        // All the work runs inside this block so teardown (detach, SPI restore, guard::end)
        // happens exactly once on every exit path.
        let outcome: anyhow::Result<()> = 'work: {
            if !attempt(15) {
                break 'work Err(if crate::guard::aborted() {
                    anyhow::anyhow!("Создание сквада отменено (F12)")
                } else {
                    anyhow::anyhow!("Окно Squad не удалось вывести в фокус — создание сквада пропущено")
                });
            }
            crate::guard::pump(100);

            // UE maps VK codes via the game thread's active layout; on RU/UK layout VK_OEM_3
            // isn't EKeys::Tilde so the console never opens. Switch Squad to EN-US, then back.
            let en_hkl = LoadKeyboardLayoutW(windows::core::w!("00000409"), KLF_ACTIVATE)
                .unwrap_or_default();
            let old_hkl = GetKeyboardLayout(squad_tid);
            let _ = PostMessageW(target, WM_INPUTLANGCHANGEREQUEST, WPARAM(0), LPARAM(en_hkl.0 as isize));
            let _ = PostMessageW(target, WM_INPUTLANGCHANGE,        WPARAM(0), LPARAM(en_hkl.0 as isize));
            crate::guard::pump(150);

            // Open console: VK_OEM_3 (0xC0) verbatim so UE always sees it.
            let _ = SendInput(&[vk_event(0xC0, 0x29, false), vk_event(0xC0, 0x29, true)],
                              size_of::<INPUT>() as i32);
            crate::guard::pump(1500);
            if crate::guard::aborted() {
                break 'work Err(anyhow::anyhow!("Создание сквада отменено (F12)"));
            }

            // Type the command with real VK codes (UE's Slate console filters VK_PACKET).
            const VK_SHIFT: u16 = 0x10;
            for ch in "CreateSquad 12 0".chars() {
                if crate::guard::aborted() {
                    break 'work Err(anyhow::anyhow!("Создание сквада отменено (F12)"));
                }
                let (vk, shift) = match ch {
                    'A'..='Z' => (ch as u16, true),
                    'a'..='z' => (ch.to_ascii_uppercase() as u16, false),
                    '0'..='9' => (ch as u16, false),
                    ' '       => (0x20, false),
                    _         => continue,
                };
                let mut evs: Vec<INPUT> = Vec::with_capacity(4);
                if shift { evs.push(vk_event(VK_SHIFT, 0x2A, false)); }
                evs.push(vk_event(vk, 0, false));
                evs.push(vk_event(vk, 0, true));
                if shift { evs.push(vk_event(VK_SHIFT, 0x2A, true)); }
                let _ = SendInput(&evs, size_of::<INPUT>() as i32);
                crate::guard::pump(30);
            }
            crate::guard::pump(100);
            let _ = SendInput(&[vk_event(0x0D, 0x1C, false), vk_event(0x0D, 0x1C, true)],
                              size_of::<INPUT>() as i32);
            crate::guard::pump(100);

            // Restore Squad's original keyboard layout.
            let _ = PostMessageW(target, WM_INPUTLANGCHANGEREQUEST, WPARAM(0), LPARAM(old_hkl.0 as isize));
            let _ = PostMessageW(target, WM_INPUTLANGCHANGE,        WPARAM(0), LPARAM(old_hkl.0 as isize));
            Ok(())
        };

        // Teardown — always.
        let _ = AttachThreadInput(our_tid, squad_tid, BOOL(0));
        let _ = SystemParametersInfoW(SPI_SETFOREGROUNDLOCKTIMEOUT, old_timeout, std::ptr::null_mut(), 0);
        crate::guard::end(&overlays);
        return outcome;
    }
}
