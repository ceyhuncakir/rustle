// Flow - the Shell half of a local dictation stack.
//
// This process owns everything that only the compositor can do: the floating
// island, reading which window has focus, and injecting text. All of it is
// exposed on the session bus so the daemon (flowd) - which owns audio capture,
// speech recognition and the cleanup model - stays an ordinary user process
// with no special privileges.

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

import {Island} from './island.js';
import {Injector, focusContext} from './injector.js';

// Holding the shortcut for at least this long means push-to-talk; anything
// shorter is a tap that latches recording on until the next tap.
const HOLD_THRESHOLD_MS = 350;
const POLL_MS = 40;
const DEBOUNCE_MS = 150;

const EXTENSION_VERSION = '6';
const BUS_NAME = 'ai.flow.Island';
const OBJECT_PATH = '/ai/flow/Island';

const IFACE = `
<node>
  <interface name="ai.flow.Island">
    <method name="Show"/>
    <method name="Hide"/>
    <method name="SetState">
      <arg type="s" name="state" direction="in"/>
    </method>
    <method name="SetText">
      <arg type="s" name="text" direction="in"/>
    </method>
    <method name="PushLevel">
      <arg type="d" name="level" direction="in"/>
    </method>
    <method name="InsertText">
      <arg type="s" name="text" direction="in"/>
    </method>
    <method name="GetFocusContext">
      <arg type="a{ss}" name="context" direction="out"/>
    </method>
    <method name="DevHideOverview"/>
    <method name="DevPressHotkey"/>
    <property name="State" type="s" access="read"/>
    <property name="Version" type="s" access="read"/>
    <signal name="HotkeyPressed">
      <arg type="s" name="mode"/>
    </signal>
    <signal name="HotkeyReleased"/>
    <signal name="CancelRequested"/>
  </interface>
</node>`;

export default class FlowExtension extends Extension {
    enable() {
        this._enableDevMode();
        this._island = new Island();
        this._injector = new Injector();
        this._recording = false;
        this._heldMask = 0;
        this._pollId = 0;
        this._lastHotkeyMs = 0;
        this._pressedAtMs = 0;

        this._dbus = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
        this._dbus.export(Gio.DBus.session, OBJECT_PATH);
        this._nameId = Gio.bus_own_name(
            Gio.BusType.SESSION,
            BUS_NAME,
            Gio.BusNameOwnerFlags.REPLACE,
            null, null, null);

        this._settings = this.getSettings();
        const action = Main.wm.addKeybinding(
            'toggle-dictation',
            this._settings,
            Meta.KeyBindingFlags.IGNORE_AUTOREPEAT,
            Shell.ActionMode.NORMAL | Shell.ActionMode.OVERVIEW,
            () => this._onHotkey());

        // Cancel: stop recording and hide without pasting. Escape alone
        // cannot be grabbed globally without breaking every app, so it is a
        // modifier chord, <Super><Control>Escape by default: Mutter already
        // owns <Super>Escape (restore shortcuts) and <Super><Shift>Escape
        // (cancel input capture).
        const cancel = Main.wm.addKeybinding(
            'cancel-dictation',
            this._settings,
            Meta.KeyBindingFlags.IGNORE_AUTOREPEAT,
            Shell.ActionMode.NORMAL | Shell.ActionMode.OVERVIEW,
            () => this._onCancel());
        if (cancel === Meta.KeyBindingAction.NONE)
            console.error('flow: could not register the cancel shortcut');

        if (action === Meta.KeyBindingAction.NONE) {
            console.error('flow: could not register the dictation shortcut - ' +
                'another application may already own it');
        } else {
            console.log(`flow: dictation shortcut registered ` +
                `(${this._settings.get_strv('toggle-dictation').join(', ')}); ` +
                `hold>=${HOLD_THRESHOLD_MS}ms = push-to-talk, ` +
                `shorter = tap-to-latch, debounce ${DEBOUNCE_MS}ms`);
        }
    }

    disable() {
        this._disableDevMode();
        this._stopModifierWatch();
        Main.wm.removeKeybinding('toggle-dictation');
        Main.wm.removeKeybinding('cancel-dictation');
        this._settings = null;
        this._recording = false;

        if (this._nameId) {
            Gio.bus_unown_name(this._nameId);
            this._nameId = 0;
        }
        this._dbus?.unexport();
        this._dbus = null;

        this._injector?.destroy();
        this._injector = null;

        this._island?.destroy();
        this._island = null;
    }

    // -- hotkey --------------------------------------------------------------
    //
    // One shortcut, two behaviours. Which one you get is decided by how long
    // the modifier stays down AFTER the shortcut fires:
    //
    //   released quickly  -> a tap: keep recording, stop on the next tap
    //   held then released -> push-to-talk: stop the moment it comes up
    //
    // Deciding at fire time does not work: the modifier is always still down
    // then, so every press would look like a hold and the tap mode could never
    // be reached. Mutter delivers no key-release event to an extension, hence
    // the poll.

    _modifierMask() {
        const [, , mods] = global.get_pointer();
        return mods & (
            Clutter.ModifierType.SHIFT_MASK |
            Clutter.ModifierType.CONTROL_MASK |
            Clutter.ModifierType.MOD1_MASK |
            Clutter.ModifierType.MOD4_MASK);
    }

    _nowMs() {
        return GLib.get_monotonic_time() / 1000;
    }

    _onHotkey() {
        // IGNORE_AUTOREPEAT should make this unnecessary, but a shortcut that
        // fires 30 times a second thrashes the daemon badly enough to be worth
        // a second line of defence.
        const now = this._nowMs();
        if (now - this._lastHotkeyMs < DEBOUNCE_MS)
            return;
        this._lastHotkeyMs = now;

        if (this._recording) {
            this._stopDictation();
            return;
        }

        this._recording = true;
        this._pressedAtMs = now;
        this._heldMask = this._modifierMask();

        this.emitHotkeyPressed('dictating');

        if (this._settings?.get_boolean('push-to-talk') && this._heldMask !== 0)
            this._watchModifierRelease();
    }

    _watchModifierRelease() {
        this._stopModifierWatch();
        this._pollId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, POLL_MS, () => {
            if ((this._modifierMask() & this._heldMask) !== 0)
                return GLib.SOURCE_CONTINUE;

            this._pollId = 0;

            // Held long enough to be deliberate: stop now. Otherwise it was a
            // tap, so leave it recording and wait for the next press.
            if (this._nowMs() - this._pressedAtMs >= HOLD_THRESHOLD_MS)
                this._stopDictation();

            return GLib.SOURCE_REMOVE;
        });
    }

    _stopModifierWatch() {
        if (this._pollId) {
            GLib.source_remove(this._pollId);
            this._pollId = 0;
        }
    }

    _onCancel() {
        this._stopModifierWatch();
        this._recording = false;
        this.emitCancelRequested();
        this._island?.setState('hidden');
    }

    _stopDictation() {
        this._stopModifierWatch();
        if (!this._recording)
            return;
        this._recording = false;
        this.emitHotkeyReleased();
    }

    // -- development only ---------------------------------------------------

    /**
     * FLOW_DEV=1 turns on Mutter's unsafe mode, which lifts the restriction on
     * org.gnome.Shell.Screenshot so scripts/capture.py can shoot every island
     * state automatically.
     *
     * The variable is set by scripts/nested-shell.sh and nothing else, so a
     * normal login never takes this path. This lives here rather than in a
     * second extension because enabling an extension writes to dconf, which a
     * nested Shell shares with the real session - a dev-only helper would end
     * up enabled at your next real login.
     */
    _enableDevMode() {
        if (GLib.getenv('FLOW_DEV') !== '1')
            return;

        this._unsafeModeWas = global.context.unsafe_mode;
        global.context.unsafe_mode = true;
        console.warn('flow: FLOW_DEV=1 - unsafe mode ON (nested dev session only)');
    }

    /**
     * Dismiss the Overview. Test-only.
     *
     * A headless Shell boots into the Overview and activating a window does
     * not dismiss it, so synthesised keys are swallowed by the search entry -
     * which lives inside gnome-shell, and would make an injection test pass
     * without ever crossing a process boundary. Shell JS is ESM in GNOME 48,
     * so Eval cannot reach Main; an extension can.
     */
    DevHideOverview() {
        if (GLib.getenv('FLOW_DEV') !== '1')
            return;
        this._guard(() => Main.overview.hide());
    }

    /** Fire the hotkey handler directly. Test-only. */
    DevPressHotkey() {
        if (GLib.getenv('FLOW_DEV') !== '1')
            return;
        this._guard(() => this._onHotkey());
    }

    _disableDevMode() {
        if (this._unsafeModeWas === undefined)
            return;

        global.context.unsafe_mode = this._unsafeModeWas;
        this._unsafeModeWas = undefined;
    }

    // -- D-Bus surface ------------------------------------------------------
    //
    // Every method is guarded: an exception raised on the bus would otherwise
    // propagate into the Shell's main loop and can take down the session.

    // Lets the daemon detect a mismatch between its expectations and the
    // extension actually loaded. Bump with metadata.json.
    get Version() {
        return EXTENSION_VERSION;
    }

    get State() {
        return this._island?.state ?? 'hidden';
    }

    Show() {
        this._guard(() => this._island.setState('idle'));
    }

    Hide() {
        this._guard(() => this._island.setState('hidden'));
    }

    SetState(state) {
        // The daemon decides whether a take is running: it drops a press
        // while it is still busy or when the microphone fails, and a take can
        // start without the extension (`flow hotkey`). Following its state
        // keeps the next tap meaning start or stop as the user expects.
        this._recording = state === 'listening';
        this._guard(() => this._island.setState(state));
    }

    SetText(text) {
        this._guard(() => this._island.setText(text));
    }

    PushLevel(level) {
        this._guard(() => this._island.pushLevel(level));
    }

    // Not guarded, unlike the calls above: the daemon waits for this reply,
    // and Gio logs an exception thrown here and returns it to the caller as
    // a D-Bus error, where a swallowed one would lose the dictation without
    // a trace. The paste itself finishes after the reply; insertText only
    // schedules it.
    InsertText(text) {
        this._injector.insertText(text);
    }

    GetFocusContext() {
        try {
            return focusContext();
        } catch (e) {
            console.error(`flow: focus context failed: ${e}`);
            return {app: '', title: '', role: ''};
        }
    }

    emitHotkeyPressed(mode) {
        this._dbus?.emit_signal('HotkeyPressed', new GLib.Variant('(s)', [mode]));
    }

    emitHotkeyReleased() {
        this._dbus?.emit_signal('HotkeyReleased', null);
    }

    emitCancelRequested() {
        this._dbus?.emit_signal('CancelRequested', null);
    }

    _guard(fn) {
        try {
            fn();
        } catch (e) {
            console.error(`flow: ${e}`);
        }
    }
}
