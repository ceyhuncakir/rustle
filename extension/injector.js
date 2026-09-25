// Text injection and focus context.
//
// On GNOME Wayland no ordinary client can type into another window: wtype
// needs zwp_virtual_keyboard_v1, which Mutter does not expose, and ydotool
// needs root access to /dev/uinput. Running inside the Shell we can use
// Mutter's *own* virtual input device, which needs no privileges at all and
// works in every client, Wayland or XWayland.
//
// Strategy: put the text on the clipboard and synthesise Ctrl+V (Ctrl+Shift+V
// in a terminal). Pasting is atomic and O(1) regardless of length, where
// synthesising one key event per character is slow and mangles dead keys and
// non-Latin layouts. The previous clipboard text is restored afterwards.

import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';
import St from 'gi://St';

import * as Keyboard from 'resource:///org/gnome/shell/ui/status/keyboard.js';

import {isNonLatinLayout} from './layouts.js';
import {isTerminal} from './terminals.js';

// Let the clipboard manager observe the new offer before pasting; pasting in
// the same frame races the selection ownership change.
const PASTE_DELAY_MS = 40;
// How long the target app gets to read the clipboard before the previous
// contents go back.
const RESTORE_DELAY_MS = 400;
// A tap on the shortcut stops recording while its modifier is still down, and
// a short take can come back from the daemon before the finger is up: the app
// would see Super+Ctrl+V. So the paste waits for the modifiers to be let go,
// polling this often, and goes ahead anyway after the longest wait.
const MODIFIER_POLL_MS = 15;
const MODIFIER_WAIT_MS = 1000;

// Linux evdev codes for the keycode chord on layouts without a 'v'.
const KEY_LEFTCTRL = 29;
const KEY_LEFTSHIFT = 42;
const KEY_V = 47;

// Modifiers a finger can be holding. Caps Lock and Num Lock are latched, not
// held, so they are left out.
const HELD_MODIFIERS =
    Clutter.ModifierType.SHIFT_MASK |
    Clutter.ModifierType.CONTROL_MASK |
    Clutter.ModifierType.MOD1_MASK |
    Clutter.ModifierType.MOD4_MASK |
    Clutter.ModifierType.MOD5_MASK |
    Clutter.ModifierType.SUPER_MASK |
    Clutter.ModifierType.HYPER_MASK |
    Clutter.ModifierType.META_MASK;

// Mutter's virtual device timestamps are monotonic microseconds, the same
// clock g_get_monotonic_time() reads. Passing milliseconds here makes Mutter
// discard the events as out-of-order.
function nowMicros() {
    return GLib.get_monotonic_time();
}

function heldModifiers() {
    const [, , mods] = global.get_pointer();
    return mods & HELD_MODIFIERS;
}

export class Injector {
    constructor() {
        // GNOME 51 removed Clutter.get_default_backend(); the stage's context
        // has carried the backend since 47, the oldest Shell this supports.
        this._seat = global.stage.get_context().get_backend().get_default_seat();
        this._device = this._seat.create_virtual_device(
            Clutter.InputDeviceType.KEYBOARD_DEVICE);
        this._clipboard = St.Clipboard.get_default();
        this._sources = new Set();
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

    /** The same by evdev keycode, which works whatever the layout. */
    tapKeycodeWithModifiers(keycode, modifiers = []) {
        for (const mod of modifiers)
            this._device.notify_key(nowMicros(), mod, Clutter.KeyState.PRESSED);

        this._device.notify_key(nowMicros(), keycode, Clutter.KeyState.PRESSED);
        this._device.notify_key(nowMicros(), keycode, Clutter.KeyState.RELEASED);

        for (const mod of [...modifiers].reverse())
            this._device.notify_key(nowMicros(), mod, Clutter.KeyState.RELEASED);
    }

    /**
     * Paste `text` into whatever currently has focus.
     *
     * Returns at once; the paste itself happens over the next few frames.
     * Only the text/plain clipboard is preserved; if the user had an image or
     * rich content copied, that selection is lost. Wispr Flow has the same
     * limitation.
     */
    insertText(text, {restoreClipboard = true} = {}) {
        if (!text)
            return;

        this._clipboard.get_text(St.ClipboardType.CLIPBOARD, (_clip, previous) => {
            // The read is asynchronous and cannot be cancelled, so it can
            // finish after destroy().
            if (!this._device)
                return;
            this._clipboard.set_text(St.ClipboardType.CLIPBOARD, text);

            this._timeout(PASTE_DELAY_MS, () => {
                this._whenModifiersReleased(() => {
                    this._pasteChord();
                    if (restoreClipboard && previous && previous !== text)
                        this._restoreLater(text, previous);
                });
                return GLib.SOURCE_REMOVE;
            });
        });
    }

    /** Ctrl+V, Ctrl+Shift+V for a terminal, by keyval or by keycode. */
    _pasteChord() {
        const terminal = isTerminal(global.display.focus_window);
        const source = Keyboard.getInputSourceManager().currentSource;

        if (isNonLatinLayout(source?.xkbId)) {
            const mods = terminal ? [KEY_LEFTCTRL, KEY_LEFTSHIFT] : [KEY_LEFTCTRL];
            this.tapKeycodeWithModifiers(KEY_V, mods);
        } else {
            const mods = terminal
                ? [Clutter.KEY_Control_L, Clutter.KEY_Shift_L]
                : [Clutter.KEY_Control_L];
            this.tapWithModifiers(Clutter.KEY_v, mods);
        }
    }

    _whenModifiersReleased(callback) {
        const deadline = GLib.get_monotonic_time() + MODIFIER_WAIT_MS * 1000;
        const check = () => {
            const held = heldModifiers();
            if (held !== 0 && GLib.get_monotonic_time() < deadline)
                return GLib.SOURCE_CONTINUE;

            if (held !== 0)
                console.warn(`rustle: modifiers still held (0x${held.toString(16)}), pasting anyway`);
            callback();
            return GLib.SOURCE_REMOVE;
        };
        if (check() === GLib.SOURCE_CONTINUE)
            this._timeout(MODIFIER_POLL_MS, check);
    }

    _restoreLater(ours, previous) {
        this._timeout(RESTORE_DELAY_MS, () => {
            // Only restore while the clipboard still holds our text; if the
            // user copied something meanwhile, that wins.
            this._clipboard.get_text(St.ClipboardType.CLIPBOARD, (_clip, current) => {
                if (this._device && current === ours)
                    this._clipboard.set_text(St.ClipboardType.CLIPBOARD, previous);
            });
            return GLib.SOURCE_REMOVE;
        });
    }

    /**
     * A GLib timeout that destroy() removes if it is still pending: a screen
     * lock disables the extension, and a timer firing after that would run
     * against a torn-down injector.
     */
    _timeout(ms, callback) {
        const id = GLib.timeout_add(GLib.PRIORITY_DEFAULT, ms, () => {
            let result = GLib.SOURCE_REMOVE;
            try {
                result = callback();
            } catch (e) {
                console.error(`rustle: paste failed: ${e}`);
            }
            if (result !== GLib.SOURCE_CONTINUE)
                this._sources.delete(id);
            return result;
        });
        this._sources.add(id);
        return id;
    }

    destroy() {
        for (const id of this._sources)
            GLib.source_remove(id);
        this._sources.clear();
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
