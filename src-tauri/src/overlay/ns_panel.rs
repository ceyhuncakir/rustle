//! macOS: the island as a non-activating panel, the way Spotlight-style
//! windows are made. Showing it never activates Flow or takes key focus from
//! the app being dictated into; it floats over normal and full-screen
//! windows, follows the user across Spaces and lets clicks through.
//!
//! AppKit only honours "non-activating" on an `NSPanel`, and Tauri makes an
//! `NSWindow` subclass. As tauri-nspanel does, the window's class is switched
//! in place to an `NSPanel` subclass of our own that refuses key and main
//! status. The switch is only made when it is known to be safe (see
//! [`become_panel`]); otherwise the window keeps its class and gets the same
//! level, Spaces and ordering treatment, which covers everything except
//! floating over another app's full-screen Space.
//!
//! Everything here works on a raw `NSWindow*`, so the module builds on its
//! own. Every function must run on the main thread; they do nothing
//! otherwise.

use std::ffi::{c_void, CStr};
use std::sync::OnceLock;

use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
use objc2::{sel, ClassType, MainThreadMarker};
use objc2_app_kit::{NSPanel, NSStatusWindowLevel, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask};

const PANEL_CLASS: &CStr = c"FlowOverlayPanel";

/// Configure the overlay window once, while it is still hidden. Returns
/// whether it became a real non-activating panel.
///
/// # Safety
/// `ns_window` must point to a live `NSWindow`.
pub unsafe fn prepare(ns_window: *mut c_void) -> bool {
    let Some(window) = (unsafe { window(ns_window) }) else { return false };
    let panel = unsafe { become_panel(window) };
    if panel {
        // SAFETY: `become_panel` made it an `NSPanel` subclass.
        let as_panel = unsafe { &*(window as *const NSWindow as *const NSPanel) };
        as_panel.setFloatingPanel(true);
        as_panel.setBecomesKeyOnlyIfNeeded(true);
        as_panel.setWorksWhenModal(true);
        window.setStyleMask(window.styleMask() | NSWindowStyleMask::NonactivatingPanel);
    }
    // Above normal and floating windows, like the menu bar's own extras.
    window.setLevel(NSStatusWindowLevel);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    window.setHidesOnDeactivate(false);
    window.setCanHide(false);
    window.setIgnoresMouseEvents(true);
    panel
}

/// Put the window on screen without activating the app or making it key.
///
/// # Safety
/// `ns_window` must point to a live `NSWindow`.
pub unsafe fn show(ns_window: *mut c_void) {
    if let Some(window) = unsafe { window(ns_window) } {
        window.orderFrontRegardless();
    }
}

/// # Safety
/// `ns_window` must point to a live `NSWindow`.
pub unsafe fn hide(ns_window: *mut c_void) {
    if let Some(window) = unsafe { window(ns_window) } {
        window.orderOut(None);
    }
}

/// # Safety
/// `ns_window` must be null or point to a live `NSWindow`.
unsafe fn window<'a>(ns_window: *mut c_void) -> Option<&'a NSWindow> {
    // AppKit objects are main-thread only; a stray call elsewhere is a no-op
    // rather than undefined behaviour.
    MainThreadMarker::new()?;
    unsafe { (ns_window as *const NSWindow).as_ref() }
}

/// Switch `window` to [`panel_class`] if that cannot corrupt it: not already
/// a panel, not observed through KVO (whose hidden subclass would be lost),
/// and the panel class needs no more instance storage than the object has.
unsafe fn become_panel(window: &NSWindow) -> bool {
    let current = AnyObject::class(window);
    if inherits(current, NSPanel::class()) {
        return true;
    }
    let Some(panel) = panel_class() else { return false };
    if current.name().to_bytes().starts_with(b"NSKVONotifying_")
        || !inherits(current, NSWindow::class())
        || panel.instance_size() > current.instance_size()
    {
        log::warn!(
            "overlay: {:?} cannot become a panel; it may not show over full-screen apps",
            current.name()
        );
        return false;
    }
    // SAFETY: checked above that the object is an `NSWindow` large enough
    // for the panel class, and nothing tracks its class. Tao's own subclass
    // overrides only key/main status (which ours replaces) and `sendEvent:`
    // for dragging by the background (moot for a click-through window).
    // `AnyObject::set_class` is not used: it asserts equal sizes, and Tao's
    // class may carry padding ours does not.
    unsafe { objc2::ffi::object_setClass(window as *const NSWindow as *mut AnyObject, panel) };
    true
}

fn inherits(class: &AnyClass, ancestor: &AnyClass) -> bool {
    let mut next = Some(class);
    while let Some(class) = next {
        if class == ancestor {
            return true;
        }
        next = class.superclass();
    }
    false
}

/// An `NSPanel` subclass that can never become key or main. It also has the
/// `focusable` ivar Tao's window class has, so Tao's `set_focusable` still
/// finds it (the value is ignored), and the instance sizes line up.
fn panel_class() -> Option<&'static AnyClass> {
    static CLASS: OnceLock<Option<&'static AnyClass>> = OnceLock::new();
    *CLASS.get_or_init(|| {
        if let Some(existing) = AnyClass::get(PANEL_CLASS) {
            return Some(existing);
        }
        let mut builder = ClassBuilder::new(PANEL_CLASS, NSPanel::class())?;
        builder.add_ivar::<Bool>(c"focusable");
        // SAFETY: both selectors take no arguments and return BOOL.
        unsafe {
            builder.add_method(sel!(canBecomeKeyWindow), refuse as extern "C-unwind" fn(_, _) -> _);
            builder.add_method(sel!(canBecomeMainWindow), refuse as extern "C-unwind" fn(_, _) -> _);
        }
        Some(builder.register())
    })
}

extern "C-unwind" fn refuse(_this: &AnyObject, _sel: Sel) -> Bool {
    Bool::NO
}
