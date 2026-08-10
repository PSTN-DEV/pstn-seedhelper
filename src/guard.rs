//! Windows-only input guard + fullscreen "creating squad" overlay.
//!
//! While a squad is being created we must stop the user's keystrokes from leaking into
//! Squad's console, but still let OUR injected keys through. BlockInput can't tell the two
//! apart and silently no-ops in many cases, so instead we install low-level keyboard/mouse
//! hooks: every key WE inject is tagged with `MAGIC` in dwExtraInfo and passed; everything
//! the user physically types/clicks is swallowed. F12 is the panic key — it sets ABORT.
//!
//! On top of that we show one topmost, click-through, no-activate layered window per monitor
//! painted with red diagonal stripes (magenta = transparent via colour-key) and a centred
//! "Создаётся сквад" box. The overlay never takes focus, so Squad keeps it and still receives
//! our keys.
#![cfg(windows)]

use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM, BOOL, HINSTANCE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;

/// dwExtraInfo stamp identifying keystrokes we injected (so the hook lets them pass).
pub const MAGIC: usize = 0x5EED_5EED;
const PANIC_VK: u32 = 0x7B; // VK_F12

// Colours are COLORREF (0x00BBGGRR). Tunables — bump for more presence, drop for calmer.
const DIM: u32 = 0x000A_0808; // near-black wash that gently darkens the whole screen
const PINK: u32 = 0x0096_78BE; // RGB(190,120,150) — muted dusty rose, easy on the eyes
const BOX_BG: u32 = 0x001A_1414; // #14141a panel, matches the app
const TITLE: u32 = 0x00D8_CCE8; // soft pink-white
const SUB: u32 = 0x0092_9292; // muted grey (app text-secondary)
const WIN_ALPHA: u8 = 120; // whole-overlay opacity = how strong the dim is (0..255)
const BORDER_W: i32 = 5; // dashed frame thickness
const BORDER_INSET: i32 = 12; // frame distance from the screen edge

static ABORT: AtomicBool = AtomicBool::new(false);
static KB_HOOK: AtomicIsize = AtomicIsize::new(0);
static MOUSE_HOOK: AtomicIsize = AtomicIsize::new(0);

pub fn aborted() -> bool {
    ABORT.load(Ordering::SeqCst)
}

// ── Low-level hooks ────────────────────────────────────────────────────────────

unsafe extern "system" fn kb_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        if kb.dwExtraInfo != MAGIC {
            // The user's own key — block it. Watch for the F12 panic key.
            let msg = wparam.0 as u32;
            if (msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN) && kb.vkCode == PANIC_VK {
                ABORT.store(true, Ordering::SeqCst);
            }
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let ms = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        if ms.dwExtraInfo != MAGIC {
            return LRESULT(1); // block all physical mouse input
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

unsafe fn install_hooks() {
    let hmod = GetModuleHandleW(None).unwrap_or_default();
    let hinst = HINSTANCE(hmod.0);
    if let Ok(h) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb_proc), hinst, 0) {
        KB_HOOK.store(h.0 as isize, Ordering::SeqCst);
    }
    if let Ok(h) = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), hinst, 0) {
        MOUSE_HOOK.store(h.0 as isize, Ordering::SeqCst);
    }
}

unsafe fn remove_hooks() {
    let k = KB_HOOK.swap(0, Ordering::SeqCst);
    if k != 0 {
        let _ = UnhookWindowsHookEx(HHOOK(k as *mut _));
    }
    let m = MOUSE_HOOK.swap(0, Ordering::SeqCst);
    if m != 0 {
        let _ = UnhookWindowsHookEx(HHOOK(m as *mut _));
    }
}

// ── Overlay windows ─────────────────────────────────────────────────────────────

const CLASS: PCWSTR = w!("SeedSquadOverlay");

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_PAINT {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        paint(hwnd, hdc);
        let _ = EndPaint(hwnd, &ps);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wp, lp)
}

unsafe fn paint(hwnd: HWND, hdc: HDC) {
    let mut rc = RECT::default();
    let _ = GetClientRect(hwnd, &mut rc);
    let w = rc.right - rc.left;
    let h = rc.bottom - rc.top;

    // Gentle dark wash over the whole screen (uniform WIN_ALPHA opacity).
    let bg = CreateSolidBrush(COLORREF(DIM));
    FillRect(hdc, &rc, bg);
    let _ = DeleteObject(bg);

    // Pink dashed frame just inside each screen's edge.
    let lb = LOGBRUSH { lbStyle: BS_SOLID, lbColor: COLORREF(PINK), lbHatch: 0 };
    let bpen = ExtCreatePen(PS_GEOMETRIC | PS_DASH | PS_ENDCAP_FLAT, BORDER_W as u32, &lb, None);
    let nullbr = GetStockObject(NULL_BRUSH);
    let ob = SelectObject(hdc, nullbr);
    let op = SelectObject(hdc, bpen);
    let _ = RoundRect(hdc, BORDER_INSET, BORDER_INSET, w - BORDER_INSET, h - BORDER_INSET, 48, 48);
    SelectObject(hdc, ob);
    SelectObject(hdc, op);
    let _ = DeleteObject(bpen);

    // Centre panel + text on the primary monitor only.
    if GetWindowLongPtrW(hwnd, GWLP_USERDATA) != 0 {
        let (cx, cy) = (w / 2, h / 2);
        let (bw, bh) = (660, 176);
        let (l, t, r, b) = (cx - bw / 2, cy - bh / 2, cx + bw / 2, cy + bh / 2);

        // Rounded dark panel with a thin pink border (app-style).
        let fill = CreateSolidBrush(COLORREF(BOX_BG));
        let border = CreatePen(PS_SOLID, 2, COLORREF(PINK));
        let ob = SelectObject(hdc, fill);
        let op = SelectObject(hdc, border);
        let _ = RoundRect(hdc, l, t, r, b, 28, 28);
        SelectObject(hdc, ob);
        SelectObject(hdc, op);
        let _ = DeleteObject(fill);
        let _ = DeleteObject(border);

        SetBkMode(hdc, TRANSPARENT);
        let title_font = make_font(-40, 700);
        let sub_font = make_font(-19, 500);

        let of = SelectObject(hdc, title_font);
        SetTextColor(hdc, COLORREF(TITLE));
        let mut l1: Vec<u16> = "СОЗДАЁТСЯ СКВАД".encode_utf16().collect();
        let mut r1 = RECT { left: l, top: cy - 56, right: r, bottom: cy - 6 };
        DrawTextW(hdc, &mut l1, &mut r1, DT_CENTER | DT_VCENTER | DT_SINGLELINE);

        SelectObject(hdc, sub_font);
        SetTextColor(hdc, COLORREF(SUB));
        let mut l2: Vec<u16> = "Не трогайте клавиатуру   ·   F12 — отмена".encode_utf16().collect();
        let mut r2 = RECT { left: l, top: cy + 8, right: r, bottom: cy + 54 };
        DrawTextW(hdc, &mut l2, &mut r2, DT_CENTER | DT_VCENTER | DT_SINGLELINE);

        SelectObject(hdc, of);
        let _ = DeleteObject(title_font);
        let _ = DeleteObject(sub_font);
    }
}

/// JetBrains Mono at the given (negative = pixel) height and weight, matching the app UI.
unsafe fn make_font(height: i32, weight: i32) -> HFONT {
    CreateFontW(
        height, 0, 0, 0, weight, 0, 0, 0,
        DEFAULT_CHARSET.0 as u32, OUT_TT_PRECIS.0 as u32,
        CLIP_DEFAULT_PRECIS.0 as u32, CLEARTYPE_QUALITY.0 as u32,
        0, w!("JetBrains Mono"),
    )
}

unsafe extern "system" fn mon_cb(hmon: HMONITOR, _hdc: HDC, _rc: *mut RECT, lparam: LPARAM) -> BOOL {
    let hwnds = &mut *(lparam.0 as *mut Vec<HWND>);
    let mut mi = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
    if !GetMonitorInfoW(hmon, &mut mi).as_bool() {
        return BOOL(1);
    }
    let r = mi.rcMonitor;
    let primary = (mi.dwFlags & MONITORINFOF_PRIMARY) != 0;
    let hmod = GetModuleHandleW(None).unwrap_or_default();
    let hinst = HINSTANCE(hmod.0);
    let ex = WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW;
    let hwnd = CreateWindowExW(
        ex, CLASS, w!(""), WS_POPUP,
        r.left, r.top, r.right - r.left, r.bottom - r.top,
        None, None, hinst, None,
    )
    .unwrap_or_default();
    if hwnd.0.is_null() {
        return BOOL(1);
    }
    SetWindowLongPtrW(hwnd, GWLP_USERDATA, if primary { 1 } else { 0 });
    let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), WIN_ALPHA, LWA_ALPHA);
    let _ = SetWindowPos(hwnd, HWND_TOPMOST, r.left, r.top, r.right - r.left, r.bottom - r.top,
        SWP_NOACTIVATE | SWP_SHOWWINDOW);
    let _ = UpdateWindow(hwnd);
    hwnds.push(hwnd);
    BOOL(1)
}

unsafe fn create_overlays() -> Vec<HWND> {
    let hmod = GetModuleHandleW(None).unwrap_or_default();
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wnd_proc),
        hInstance: HINSTANCE(hmod.0),
        lpszClassName: CLASS,
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
        ..Default::default()
    };
    RegisterClassW(&wc); // 0 if already registered — fine
    let mut hwnds: Vec<HWND> = Vec::new();
    let _ = EnumDisplayMonitors(HDC::default(), None, Some(mon_cb), LPARAM(&mut hwnds as *mut _ as isize));
    hwnds
}

// ── Public API ──────────────────────────────────────────────────────────────────

/// Start the guard: reset abort, install hooks, show overlays. Returns the overlay windows
/// to pass back to [`end`]. Must be called on the same thread that will run the work and
/// [`pump`] the message queue (LL hooks require it).
pub unsafe fn begin() -> Vec<HWND> {
    ABORT.store(false, Ordering::SeqCst);
    install_hooks();
    create_overlays()
}

/// Tear down: remove hooks and destroy the overlay windows.
pub unsafe fn end(overlays: &[HWND]) {
    remove_hooks();
    for &h in overlays {
        let _ = DestroyWindow(h);
    }
}

/// Debug-only: show just the overlay (no hooks, so your input still works) until Esc or
/// 20s. Lets you eyeball the look without running a real seed. Run: `--preview-overlay`.
#[cfg(debug_assertions)]
pub unsafe fn preview() {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    let overlays = create_overlays();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // F12 (the real panic key) or Esc closes the preview.
        let down = |vk: i32| (GetAsyncKeyState(vk) as u16 & 0x8000) != 0;
        if down(PANIC_VK as i32) || down(0x1B) || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    for &h in &overlays {
        let _ = DestroyWindow(h);
    }
}

/// Sleep `ms` while pumping the message queue so the hooks stay live (LL hooks are dropped
/// if the installing thread stops responding) and the overlays keep painting. Returns early
/// if the user pressed the F12 panic key.
pub unsafe fn pump(ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    loop {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if ABORT.load(Ordering::SeqCst) {
            return;
        }
        let now = Instant::now();
        if now >= end {
            return;
        }
        std::thread::sleep(Duration::from_millis(5).min(end - now));
    }
}

// POINT is referenced by MSLLHOOKSTRUCT; keep the import used on all toolchains.
const _: fn() -> POINT = POINT::default;
