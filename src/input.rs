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
        if let Err(e) = windows_create_squad(log) {
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
fn windows_create_squad(log: &crate::app::LogSender) -> anyhow::Result<()> {
    use std::mem::size_of;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, VIRTUAL_KEY,
        KEYEVENTF_KEYUP, LoadKeyboardLayoutW, GetKeyboardLayout,
        KLF_ACTIVATE,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM, BOOL};

    macro_rules! log {
        ($($arg:tt)*) => { let _ = log.send(format!("{}", format!($($arg)*))); };
    }

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
    // log!("Окно Squad найдено");

    // Sends a virtual-key INPUT event. wVk is passed through directly to WM_KEYDOWN wParam
    // without any layout translation — layout only matters when KEYEVENTF_SCANCODE is set.
    // This lets us force VK_OEM_3 (backtick key) regardless of active keyboard layout.
    let vk_event = |vk: u16, scan: u16, up: bool| -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: scan,
                    dwFlags: if up { KEYEVENTF_KEYUP } else { windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(0) },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    };

    unsafe {
        if IsIconic(target).as_bool() {
            log!("Окно Squad свёрнуто, восстанавливаем");
            let _ = ShowWindow(target, SW_RESTORE);
            std::thread::sleep(std::time::Duration::from_millis(300));
        }

        // AttachThreadInput + SetFocus moves actual keyboard focus to target.
        // SetForegroundWindow alone only changes z-order; SendInput still needs
        // the window to own keyboard focus, which SetFocus guarantees.
        let squad_tid = GetWindowThreadProcessId(target, None);
        let our_tid   = GetCurrentThreadId();
        #[link(name = "user32")]
        extern "system" {
            fn SetFocus(hwnd: HWND) -> HWND;
            fn SetActiveWindow(hwnd: HWND) -> HWND;
            fn SystemParametersInfoW(action: u32, param: u32, pvparam: *mut core::ffi::c_void, ini: u32) -> BOOL;
        }
        const SPI_GETFOREGROUNDLOCKTIMEOUT: u32 = 0x2000;
        const SPI_SETFOREGROUNDLOCKTIMEOUT: u32 = 0x2001;

        // Temporarily disable the foreground lock so SetForegroundWindow succeeds from
        // a non-foreground process. Default timeout is ~200ms; 0 means unrestricted.
        let mut old_timeout: u32 = 200;
        let _ = SystemParametersInfoW(SPI_GETFOREGROUNDLOCKTIMEOUT, 0, &mut old_timeout as *mut u32 as *mut _, 0);
        let _ = SystemParametersInfoW(SPI_SETFOREGROUNDLOCKTIMEOUT, 0, std::ptr::null_mut(), 0);

        let _ = AttachThreadInput(our_tid, squad_tid, BOOL(1));
        let _ = BringWindowToTop(target);
        let _ = SetForegroundWindow(target);
        SetActiveWindow(target);
        SetFocus(target);
        let _ = AttachThreadInput(our_tid, squad_tid, BOOL(0));

        std::thread::sleep(std::time::Duration::from_millis(500));

        // Restore system setting regardless of what happens next.
        let _ = SystemParametersInfoW(SPI_SETFOREGROUNDLOCKTIMEOUT, old_timeout, std::ptr::null_mut(), 0);

        let fg = GetForegroundWindow();
        if fg != target {
            // log!("ВНИМАНИЕ: Фокус окна Squad переключился (fg={:?}, target={:?})", fg, target);
        } else {
            // log!("Фокус окна Squad — ОК");
        }

        // WM_ACTIVATE + WM_SETFOCUS complete the activation handshake UE expects before
        // it starts routing key events to console logic.
        use windows::Win32::UI::WindowsAndMessaging::WA_ACTIVE;
        let _ = PostMessageW(target, WM_ACTIVATE, WPARAM(WA_ACTIVE as usize), LPARAM(0));
        let _ = PostMessageW(target, WM_SETFOCUS, WPARAM(0), LPARAM(0));
        std::thread::sleep(std::time::Duration::from_millis(100));

        // UE maps VK codes to its internal key names using MapVirtualKeyEx against the
        // game thread's active layout. On Russian/Ukrainian layout VK_OEM_3 doesn't map
        // to EKeys::Tilde, so the console toggle never fires.
        // Fix: temporarily switch Squad's layout to EN-US via WM_INPUTLANGCHANGE,
        // send the key, then restore the original layout.
        let en_hkl = LoadKeyboardLayoutW(windows::core::w!("00000409"), KLF_ACTIVATE)
            .unwrap_or_default();
        let old_hkl = GetKeyboardLayout(squad_tid);
        // log!("Переключаем раскладку в Squad на EN-US.");
        let _ = PostMessageW(target, WM_INPUTLANGCHANGEREQUEST, WPARAM(0), LPARAM(en_hkl.0 as isize));
        let _ = PostMessageW(target, WM_INPUTLANGCHANGE,        WPARAM(0), LPARAM(en_hkl.0 as isize));
        std::thread::sleep(std::time::Duration::from_millis(150));

        // Open console: VK_OEM_3 (0xC0) sent as wVk without KEYEVENTF_SCANCODE so Windows
        // passes it through verbatim — no layout translation, UE always sees VK_OEM_3.
        // log!("Открываем консоль.");
        let _ = SendInput(
            &[vk_event(0xC0, 0x29, false), vk_event(0xC0, 0x29, true)],
            size_of::<INPUT>() as i32,
        );
        std::thread::sleep(std::time::Duration::from_millis(1500));


        // Type command using real VK codes — same pipeline as the backtick key.
        // KEYEVENTF_UNICODE produces VK_PACKET (0xE7) which UE's Slate console may filter;
        // real VK codes are always processed. Squad has EN-US layout active so VK_x maps
        // to the correct ASCII character regardless of the user's physical layout.
        // log!("Создаем отряд.");
        const VK_SHIFT: u16 = 0x10;
        for ch in "CreateSquad 12 0".chars() {
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
            std::thread::sleep(std::time::Duration::from_millis(30));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));

        // log!("отправляем Enter");
        let _ = SendInput(
            &[vk_event(0x0D, 0x1C, false), vk_event(0x0D, 0x1C, true)],
            size_of::<INPUT>() as i32,
        );
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Restore Squad's original keyboard layout.
        let _ = PostMessageW(target, WM_INPUTLANGCHANGEREQUEST, WPARAM(0), LPARAM(old_hkl.0 as isize));
        let _ = PostMessageW(target, WM_INPUTLANGCHANGE,        WPARAM(0), LPARAM(old_hkl.0 as isize));
        // log!("Раскладка восстановлена");
    }

    Ok(())
}
