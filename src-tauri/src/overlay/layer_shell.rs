//! KDE and wlroots desktops: the island as a wlr-layer-shell surface, which
//! the compositor keeps above every window, on every workspace, and never
//! gives keyboard focus.
//!
//! gtk-layer-shell does the protocol work for a GTK3 window. It is loaded at
//! run time rather than linked, so one Linux build still starts where it is
//! missing (the island then falls back to a plain window). Everything here
//! works on a raw `GtkWindow*`, so the module builds on its own.

use std::ffi::{c_char, c_int, c_void};
use std::sync::OnceLock;

use libloading::Library;

/// What to install when the library is missing, for the log.
pub const INSTALL_HINT: &str =
    "install gtk-layer-shell (Fedora, Arch: gtk-layer-shell; Debian, Ubuntu: libgtk-layer-shell0)";

const LIBRARY_NAMES: [&str; 2] = ["libgtk-layer-shell.so.0", "libgtk-layer-shell.so"];

// From gtk-layer-shell.h.
const LAYER_OVERLAY: c_int = 3;
const EDGE_BOTTOM: c_int = 3;
const KEYBOARD_MODE_NONE: c_int = 0;
const NAMESPACE: &[u8] = b"rustle\0";

type InitFn = unsafe extern "C" fn(*mut c_void);
type IntFn = unsafe extern "C" fn(*mut c_void, c_int);
type EdgeFn = unsafe extern "C" fn(*mut c_void, c_int, c_int);
type NamespaceFn = unsafe extern "C" fn(*mut c_void, *const c_char);

/// Why the window stays a plain one.
#[derive(Debug)]
pub enum Unavailable {
    /// The library could not be loaded, or is too old.
    Library(String),
    /// Loaded, but this compositor has no wlr-layer-shell (GNOME, or GTK is
    /// running on X11).
    Compositor,
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unavailable::Library(why) => write!(f, "gtk-layer-shell unavailable ({why}); {INSTALL_HINT}"),
            Unavailable::Compositor => f.write_str("this compositor does not offer wlr-layer-shell"),
        }
    }
}

struct Api {
    is_supported: unsafe extern "C" fn() -> c_int,
    init_for_window: InitFn,
    set_namespace: NamespaceFn,
    set_layer: IntFn,
    set_anchor: EdgeFn,
    set_margin: EdgeFn,
    set_exclusive_zone: IntFn,
    /// `gtk_layer_set_keyboard_mode` (0.6+), else the older
    /// `gtk_layer_set_keyboard_interactivity`; both take 0 for "never".
    set_keyboard: IntFn,
    /// Kept loaded for the life of the process: the window holds on to it.
    _library: Library,
}

impl Api {
    fn load() -> Result<Api, String> {
        let mut errors = Vec::new();
        for name in LIBRARY_NAMES {
            // SAFETY: gtk-layer-shell has no load-time side effects beyond
            // registering its GObject types, which is safe on any thread.
            match unsafe { Library::new(name) } {
                Ok(library) => return unsafe { Api::resolve(library) },
                Err(err) => errors.push(err.to_string()),
            }
        }
        Err(errors.join("; "))
    }

    /// # Safety
    /// `library` must be gtk-layer-shell, so the symbols have these types.
    unsafe fn resolve(library: Library) -> Result<Api, String> {
        unsafe fn get<T: Copy>(library: &Library, name: &str) -> Result<T, String> {
            unsafe { library.get::<T>(name.as_bytes()) }
                .map(|symbol| *symbol)
                .map_err(|_| format!("{name} not found, gtk-layer-shell is older than 0.6"))
        }
        unsafe {
            let set_keyboard = get::<IntFn>(&library, "gtk_layer_set_keyboard_mode")
                .or_else(|_| get::<IntFn>(&library, "gtk_layer_set_keyboard_interactivity"))?;
            Ok(Api {
                is_supported: get(&library, "gtk_layer_is_supported")?,
                init_for_window: get(&library, "gtk_layer_init_for_window")?,
                set_namespace: get(&library, "gtk_layer_set_namespace")?,
                set_layer: get(&library, "gtk_layer_set_layer")?,
                set_anchor: get(&library, "gtk_layer_set_anchor")?,
                set_margin: get(&library, "gtk_layer_set_margin")?,
                set_exclusive_zone: get(&library, "gtk_layer_set_exclusive_zone")?,
                set_keyboard,
                _library: library,
            })
        }
    }
}

fn api() -> Result<&'static Api, Unavailable> {
    static API: OnceLock<Result<Api, String>> = OnceLock::new();
    API.get_or_init(Api::load).as_ref().map_err(|why| Unavailable::Library(why.clone()))
}

/// Turn `gtk_window` into an overlay-layer surface anchored to the bottom
/// edge, `bottom_margin` logical pixels up, horizontally centred by the
/// compositor, clear of panels that reserve space, and never focused.
///
/// # Safety
/// `gtk_window` must be a live `GtkWindow*` that is not realized yet, and
/// this must run on the GTK main thread.
pub unsafe fn init(gtk_window: *mut c_void, bottom_margin: i32) -> Result<(), Unavailable> {
    let api = api()?;
    unsafe {
        if (api.is_supported)() == 0 {
            return Err(Unavailable::Compositor);
        }
        (api.init_for_window)(gtk_window);
        (api.set_namespace)(gtk_window, NAMESPACE.as_ptr().cast());
        (api.set_layer)(gtk_window, LAYER_OVERLAY);
        (api.set_anchor)(gtk_window, EDGE_BOTTOM, 1);
        (api.set_margin)(gtk_window, EDGE_BOTTOM, bottom_margin);
        // 0: move out of the way of docks and panels, push nothing aside.
        (api.set_exclusive_zone)(gtk_window, 0);
        (api.set_keyboard)(gtk_window, KEYBOARD_MODE_NONE);
    }
    Ok(())
}
