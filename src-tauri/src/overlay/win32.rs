//! Windows: keep the island topmost and out of the focus chain.
//!
//! Tao already builds the window with `WS_EX_NOACTIVATE` (not focusable),
//! shows it with `SW_SHOWNOACTIVATE` (built unfocused) and adds
//! `WS_EX_TRANSPARENT | WS_EX_LAYERED` for click-through. What it lacks:
//! `WS_EX_TOOLWINDOW`, so Alt+Tab and Task View skip the pill and it is not
//! tied to one virtual desktop, and re-asserting `HWND_TOPMOST` on every
//! show, since other topmost windows can end up above it. Tao rewrites the
//! extended style from its own flags whenever one changes, so both are
//! applied again each time the window is shown.
//!
//! Everything here works on a raw `HWND`, so the module builds on its own.
//! Call these on the thread that owns the window, so nothing waits on a
//! cross-thread `SendMessage`.

use std::ffi::c_void;

use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowLongW, SetWindowLongW, SetWindowPos, GWL_EXSTYLE, HWND_TOPMOST, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, WS_EX_APPWINDOW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW,
};

/// Called once, while the window is still hidden.
///
/// # Safety
/// `hwnd` must be a valid window handle owned by the calling thread.
pub unsafe fn prepare(hwnd: *mut c_void) {
    if unsafe { add_styles(hwnd) } {
        unsafe {
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            )
        };
    }
}

/// After the window was shown: back on top of every other topmost window,
/// visible, and still not activated.
///
/// # Safety
/// `hwnd` must be a valid window handle owned by the calling thread.
pub unsafe fn raise(hwnd: *mut c_void) {
    let mut flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW;
    if unsafe { add_styles(hwnd) } {
        flags |= SWP_FRAMECHANGED;
    }
    unsafe { SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, flags) };
}

/// Returns whether the extended style had to change.
unsafe fn add_styles(hwnd: *mut c_void) -> bool {
    let current = unsafe { GetWindowLongW(hwnd, GWL_EXSTYLE) } as u32;
    let wanted = (current | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW) & !WS_EX_APPWINDOW;
    if wanted == current {
        return false;
    }
    unsafe { SetWindowLongW(hwnd, GWL_EXSTYLE, wanted as i32) };
    true
}
