// Text injection and focus context.
//
// On GNOME Wayland no ordinary client can type into another window: wtype
// needs zwp_virtual_keyboard_v1, which Mutter does not expose, and ydotool
// needs root access to /dev/uinput. Running inside the Shell we can use
// Mutter's *own* virtual input device, which needs no privileges at all and
// works in every client, Wayland or XWayland.
//
// Strategy: put the text on the clipboard and synthesise Ctrl+V. Pasting is
// atomic and O(1) regardless of length, where synthesising one key event per
// character is slow and mangles dead keys and non-Latin layouts. The previous
// clipboard text is restored afterwards.

import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import St from 'gi://St';

// Mutter's virtual device timestamps are monotonic microseconds, the same
// clock g_get_monotonic_time() reads. Passing milliseconds here makes Mutter
// discard the events as out-of-order.
function nowMicros() {
    return GLib.get_monotonic_time();
}

export class Injector {
    constructor() {
        this._seat = Clutter.get_default_backend().get_default_seat();
        this._device = this._seat.create_virtual_device(
            Clutter.InputDeviceType.KEYBOARD_DEVICE);
        this._clipboard = St.Clipboard.get_default();
    }

    /** Press and release a keyval while holding the given modifier keyvals. */
    tapWithModifiers(keyval, modifiers = []) {
        for (const mod of modifiers)
            this._device.notify_keyval(nowMicros(), mod, Clutter.KeyState.PRESSED);

        this._device.notify_keyval(nowMicros(), keyval, Clutter.KeyState.PRESSED);
        this._device.notify_keyval(nowMicros(), keyval, Clutter.KeyState.RELEASED);

        for (const mod of [...modifiers].reverse())
            this._device.notify_keyval(nowMicros(), mod, Clutter.KeyState.RELEASED);
    }

    /**
     * Paste `text` into whatever currently has focus.
     *
     * Only the text/plain clipboard is preserved; if the user had an image or
     * rich content copied, that selection is lost. Wispr Flow has the same
     * limitation.
     */
    insertText(text, {restoreClipboard = true} = {}) {
        if (!text)
            return;

        this._clipboard.get_text(St.ClipboardType.CLIPBOARD, (_clip, previous) => {
            this._clipboard.set_text(St.ClipboardType.CLIPBOARD, text);

            // Let the clipboard manager observe the new offer before pasting;
            // pasting in the same frame races the selection ownership change.
            GLib.timeout_add(GLib.PRIORITY_DEFAULT, 40, () => {
                this.tapWithModifiers(Clutter.KEY_v, [Clutter.KEY_Control_L]);

                if (restoreClipboard && previous) {
                    GLib.timeout_add(GLib.PRIORITY_DEFAULT, 400, () => {
                        this._clipboard.set_text(St.ClipboardType.CLIPBOARD, previous);
                        return GLib.SOURCE_REMOVE;
                    });
                }
                return GLib.SOURCE_REMOVE;
            });
        });
    }

    destroy() {
        this._device = null;
        this._seat = null;
    }
}

/**
 * What the user is dictating into. Only the Shell can read this on Wayland,
 * which is what lets the cleanup model adapt its tone per application.
 */
export function focusContext() {
    const win = global.display.focus_window;
    if (!win)
        return {app: '', title: '', role: ''};

    return {
        app: win.get_wm_class() ?? '',
        title: win.get_title() ?? '',
        role: win.get_role() ?? '',
    };
}
