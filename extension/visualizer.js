// The animated core of the island: a Cairo-painted St.DrawingArea that renders
// every "busy" state from one 60fps draw loop.
//
// One actor and one timer handle bars, dots and the checkmark so the states
// can cross-fade into each other instead of swapping widgets mid-animation.

import GObject from 'gi://GObject';
import St from 'gi://St';
import GLib from 'gi://GLib';
import Cairo from 'cairo';

import {PALETTE, setColor} from './theme.js';

const BAR_COUNT = 26;
const BAR_WIDTH = 3;
const BAR_GAP = 4;
const FRAME_MS = 16;

// How fast a bar chases its target. Attack is quick so speech feels immediate,
// release is slow so the waveform decays instead of snapping to zero.
const ATTACK = 0.55;
const RELEASE = 0.12;

export const Visualizer = GObject.registerClass(
class Visualizer extends St.DrawingArea {
    _init(params = {}) {
        super._init({
            style_class: 'flow-visualizer',
            x_expand: true,
            y_expand: true,
            ...params,
        });

        // Rolling window of amplitudes, newest at the end.
        this._levels = new Array(BAR_COUNT).fill(0);
        this._shown = new Array(BAR_COUNT).fill(0);
        this._mode = 'idle';
        this._phase = 0;
        this._checkProgress = 0;
        this._timerId = 0;

        this.connect('repaint', () => this._draw());
        this.connect('destroy', () => this.stop());
    }

    /** One of: idle | listening | thinking | success | error */
    setMode(mode) {
        if (this._mode === mode)
            return;
        this._mode = mode;
        this._checkProgress = 0;
        if (mode === 'listening') {
            // Clear the drawn heights too, not just the targets: otherwise the
            // first frames show the previous take's waveform decaying away.
            this._levels.fill(0);
            this._shown.fill(0);
        }
        this.queue_repaint();
    }

    /** Feed a new amplitude in 0..1 from the capture thread. */
    pushLevel(level) {
        const clamped = Math.max(0, Math.min(1, level));
        this._levels.shift();
        this._levels.push(clamped);
    }

    start() {
        if (this._timerId)
            return;
        this._timerId = GLib.timeout_add(GLib.PRIORITY_DEFAULT, FRAME_MS, () => {
            this._advance();
            this.queue_repaint();
            return GLib.SOURCE_CONTINUE;
        });
    }

    stop() {
        if (this._timerId) {
            GLib.source_remove(this._timerId);
            this._timerId = 0;
        }
    }

    _advance() {
        this._phase += FRAME_MS / 1000;
        if (this._mode === 'success' || this._mode === 'error')
            this._checkProgress = Math.min(1, this._checkProgress + 0.06);

        // Idle breathes on a slow sine so the pill never looks frozen.
        if (this._mode === 'idle' || this._mode === 'thinking') {
            for (let i = 0; i < BAR_COUNT; i++)
                this._levels[i] = 0.10 + 0.05 * Math.sin(this._phase * 2.2 - i * 0.35);
        }

        for (let i = 0; i < BAR_COUNT; i++) {
            const target = this._levels[i];
            const rate = target > this._shown[i] ? ATTACK : RELEASE;
            this._shown[i] += (target - this._shown[i]) * rate;
        }
    }

    _draw() {
        const [width, height] = this.get_surface_size();
        if (width <= 0 || height <= 0)
            return;

        const cr = this.get_context();
        cr.setLineCap(Cairo.LineCap.ROUND);
        cr.setLineWidth(BAR_WIDTH);

        switch (this._mode) {
        case 'thinking':
            this._drawDots(cr, width, height);
            break;
        case 'success':
            this._drawCheck(cr, width, height, PALETTE.success);
            break;
        case 'error':
            this._drawCross(cr, width, height, PALETTE.danger);
            break;
        default:
            this._drawBars(cr, width, height);
            break;
        }

        cr.$dispose();
    }

    _drawBars(cr, width, height) {
        const colour = this._mode === 'listening' ? PALETTE.accent : PALETTE.muted;
        const mid = height / 2;
        const maxHalf = height / 2 - BAR_WIDTH;

        // The pill shrinks the visualizer when a transcript shares the row, so
        // fit the bar count to the width we actually got. Drawing a fixed 26
        // bars into 84px overlaps them into a wedge.
        const slot = BAR_WIDTH + BAR_GAP;
        const count = Math.max(4, Math.min(BAR_COUNT, Math.floor((width + BAR_GAP) / slot)));
        const offset = BAR_COUNT - count;  // keep the most recent samples

        const span = count * BAR_WIDTH + (count - 1) * BAR_GAP;
        let x = (width - span) / 2 + BAR_WIDTH / 2;

        for (let i = 0; i < count; i++) {
            // Taper the ends so the waveform fades out rather than cutting off.
            const edge = Math.min(i, count - 1 - i) / (count / 4);
            const taper = Math.min(1, edge);
            const half = Math.max(BAR_WIDTH / 2, this._shown[offset + i] * maxHalf * taper);

            setColor(cr, colour, 0.35 + 0.65 * taper);
            cr.moveTo(x, mid - half);
            cr.lineTo(x, mid + half);
            cr.stroke();
            x += slot;
        }
    }

    _drawDots(cr, width, height) {
        const count = 3;
        const radius = 3.5;
        const gap = 12;
        const span = (count - 1) * gap;
        const mid = height / 2;
        let x = (width - span) / 2;

        for (let i = 0; i < count; i++) {
            // Each dot rides the same sine, offset, so they chase one another.
            const wave = Math.sin(this._phase * 4.5 - i * 0.9);
            const scale = 0.65 + 0.35 * (wave + 1) / 2;
            setColor(cr, PALETTE.thinking, 0.45 + 0.55 * (wave + 1) / 2);
            cr.arc(x, mid, radius * scale, 0, 2 * Math.PI);
            cr.fill();
            x += gap;
        }
    }

    _drawCheck(cr, width, height, colour) {
        const t = this._checkProgress;
        const cx = width / 2;
        const cy = height / 2;
        const s = Math.min(width, height) * 0.22;

        // Two strokes drawn in sequence: short down-leg, then the long up-leg.
        const p0 = [cx - s, cy];
        const p1 = [cx - s * 0.25, cy + s * 0.7];
        const p2 = [cx + s, cy - s * 0.65];

        setColor(cr, colour, 1.0);
        cr.setLineWidth(3.5);
        cr.moveTo(p0[0], p0[1]);

        const legOne = Math.min(1, t / 0.4);
        cr.lineTo(p0[0] + (p1[0] - p0[0]) * legOne, p0[1] + (p1[1] - p0[1]) * legOne);

        if (t > 0.4) {
            const legTwo = Math.min(1, (t - 0.4) / 0.6);
            cr.lineTo(p1[0] + (p2[0] - p1[0]) * legTwo, p1[1] + (p2[1] - p1[1]) * legTwo);
        }
        cr.stroke();
    }

    _drawCross(cr, width, height, colour) {
        const t = this._checkProgress;
        const cx = width / 2;
        const cy = height / 2;
        const s = Math.min(width, height) * 0.18;

        setColor(cr, colour, 1.0);
        cr.setLineWidth(3.5);

        const a = Math.min(1, t / 0.5);
        cr.moveTo(cx - s, cy - s);
        cr.lineTo(cx - s + 2 * s * a, cy - s + 2 * s * a);
        cr.stroke();

        if (t > 0.5) {
            const b = Math.min(1, (t - 0.5) / 0.5);
            cr.moveTo(cx + s, cy - s);
            cr.lineTo(cx + s - 2 * s * b, cy - s + 2 * s * b);
            cr.stroke();
        }
    }
});
