// The island itself: a floating pill that morphs between dictation states.
//
// It lives in the Shell's top chrome with affectsInputRegion disabled, so it
// floats above every window and never takes focus or swallows a click — which
// is the whole reason this has to be a Shell extension rather than an app
// window. Mutter has no wlr-layer-shell, so no ordinary client can do this.

import GObject from 'gi://GObject';
import St from 'gi://St';
import Clutter from 'gi://Clutter';
import GLib from 'gi://GLib';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {Visualizer} from './visualizer.js';
import {GEOMETRY, TIMING} from './theme.js';

const STATES = ['hidden', 'idle', 'listening', 'thinking', 'inserting', 'error'];

// Width the visualizer keeps when a transcript shares the pill with it.
const VIS_WIDTH_WITH_TEXT = 84;
const TEXT_PADDING = 34;

export const Island = GObject.registerClass(
class Island extends St.BoxLayout {
    _init() {
        super._init({
            style_class: 'flow-island',
            reactive: false,
            track_hover: false,
            can_focus: false,
            visible: false,
            opacity: 0,
        });

        this.set_pivot_point(0.5, 0.5);

        this._visualizer = new Visualizer();
        this.add_child(this._visualizer);

        this._label = new St.Label({
            style_class: 'flow-island-label',
            y_align: Clutter.ActorAlign.CENTER,
            visible: false,
        });
        this._label.clutter_text.ellipsize = 3; // Pango.EllipsizeMode.END
        this._label.clutter_text.single_line_mode = true;
        this.add_child(this._label);

        this._state = 'hidden';
        this._text = '';
        this._hideTimer = 0;

        // Keep the pill centred while its width animates between states.
        this.connect('notify::width', () => this._reposition());
        this._monitorsId = Main.layoutManager.connect('monitors-changed',
            () => this._reposition());

        Main.layoutManager.addTopChrome(this, {
            affectsInputRegion: false,
            affectsStruts: false,
            trackFullscreen: false,
        });
    }

    get state() {
        return this._state;
    }

    setState(state) {
        if (!STATES.includes(state))
            throw new Error(`unknown island state: ${state}`);

        this._clearHideTimer();

        if (state === 'hidden') {
            this._state = 'hidden';
            this._animateOut();
            return;
        }

        const wasHidden = this._state === 'hidden';
        this._state = state;

        this._visualizer.setMode(state === 'inserting' ? 'success' : state);

        if (wasHidden) {
            // _animateIn already sizes the pill correctly, so morphing to the
            // same value only adds a redundant transition that repositions on
            // every frame.
            this._animateIn();
        } else {
            this._morph();
        }

        // The success state is a confirmation, not a mode - it clears itself.
        if (state === 'inserting') {
            this._hideTimer = GLib.timeout_add(GLib.PRIORITY_DEFAULT,
                TIMING.autoHide, () => {
                    this._hideTimer = 0;
                    this.setState('hidden');
                    return GLib.SOURCE_REMOVE;
                });
        }
    }

    setText(text) {
        this._text = text ?? '';
        const hasText = this._text.length > 0;
        this._label.text = this._text;
        this._label.visible = hasText;
        this._visualizer.x_expand = !hasText;
        if (this._state !== 'hidden')
            this._morph();
    }

    pushLevel(level) {
        this._visualizer.pushLevel(level);
    }

    /** Target pill geometry for the current state and text. */
    _targetSize() {
        const base = GEOMETRY.widths[this._state] ?? GEOMETRY.widths.idle;
        if (!this._text)
            return [base, GEOMETRY.height];

        const monitor = Main.layoutManager.primaryMonitor;
        const maxWidth = monitor ? Math.round(monitor.width * 0.55) : 720;
        const [, natural] = this._label.get_preferred_width(-1);
        const width = VIS_WIDTH_WITH_TEXT + natural + TEXT_PADDING;

        return [Math.max(base, Math.min(width, maxWidth)), GEOMETRY.heightExpanded];
    }

    _morph() {
        const [width, height] = this._targetSize();
        this._visualizer.width = this._text ? VIS_WIDTH_WITH_TEXT : -1;

        this.ease({
            width,
            height,
            duration: TIMING.morph,
            mode: Clutter.AnimationMode.EASE_OUT_EXPO,
        });
    }

    _animateIn() {
        this._visualizer.start();
        this.show();
        this.remove_all_transitions();

        const [width, height] = this._targetSize();
        this.set_size(width, height);
        this._reposition();

        this.set_scale(0.88, 0.88);
        this.translation_y = 18;
        this.opacity = 0;

        // Opacity and geometry must ease on different curves. EASE_OUT_BACK
        // overshoots its target on purpose - that overshoot is the pop that
        // makes the island feel physical - but opacity is a guint8 capped at
        // 255, so overshooting wraps it round to near zero and the pill
        // flashes transparent just as it should be solid.
        this.ease({
            opacity: 255,
            duration: TIMING.settle,
            mode: Clutter.AnimationMode.EASE_OUT_QUAD,
        });

        this.ease({
            scale_x: 1,
            scale_y: 1,
            translation_y: 0,
            duration: TIMING.settle,
            mode: Clutter.AnimationMode.EASE_OUT_BACK,
        });
    }

    _animateOut() {
        this.remove_all_transitions();
        this.ease({
            opacity: 0,
            scale_x: 0.9,
            scale_y: 0.9,
            translation_y: 14,
            duration: TIMING.fade,
            mode: Clutter.AnimationMode.EASE_IN_QUAD,
            onComplete: () => {
                this.hide();
                this._visualizer.stop();
                this._visualizer.setMode('idle');
                this.setText('');
            },
        });
    }

    _reposition() {
        const monitor = Main.layoutManager.primaryMonitor;
        if (!monitor)
            return;

        // Sit above the work area so the pill clears a bottom dock or panel.
        const work = Main.layoutManager.getWorkAreaForMonitor(monitor.index);
        const x = Math.round(monitor.x + (monitor.width - this.width) / 2);
        const y = Math.round(work.y + work.height - this.height - GEOMETRY.bottomMargin);
        this.set_position(x, y);
    }

    _clearHideTimer() {
        if (this._hideTimer) {
            GLib.source_remove(this._hideTimer);
            this._hideTimer = 0;
        }
    }

    destroy() {
        this._clearHideTimer();
        if (this._monitorsId) {
            Main.layoutManager.disconnect(this._monitorsId);
            this._monitorsId = 0;
        }
        this._visualizer.stop();
        Main.layoutManager.removeChrome(this);
        super.destroy();
    }
});
