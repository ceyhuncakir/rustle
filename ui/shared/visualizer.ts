// The animated core of the island, ported from extension/visualizer.js: one
// canvas and one 16 ms timer render bars, dots, the checkmark and the cross,
// so the states can cross-fade into each other instead of swapping widgets.

import { PALETTE, type VisualizerMode } from "./theme";

const BAR_COUNT = 26;
const BAR_WIDTH = 3;
const BAR_GAP = 4;
const FRAME_MS = 16;

// How fast a bar chases its target. Attack is quick so speech feels immediate,
// release is slow so the waveform decays instead of snapping to zero.
const ATTACK = 0.55;
const RELEASE = 0.12;

function withAlpha(hex: string, alpha: number): string {
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}

export class Visualizer {
  readonly canvas: HTMLCanvasElement;
  private ctx: CanvasRenderingContext2D;
  private levels = new Array<number>(BAR_COUNT).fill(0);
  private shown = new Array<number>(BAR_COUNT).fill(0);
  private mode: VisualizerMode = "idle";
  private phase = 0;
  private checkProgress = 0;
  private timer = 0;
  private observer: ResizeObserver | null = null;
  // CSS pixel size of the drawing surface; the backing store is DPR-scaled.
  private width = 0;
  private height = 0;

  constructor(canvas: HTMLCanvasElement) {
    this.canvas = canvas;
    const ctx = canvas.getContext("2d");
    if (!ctx) throw new Error("2d canvas context unavailable");
    this.ctx = ctx;

    if (typeof ResizeObserver !== "undefined") {
      this.observer = new ResizeObserver(() => this.resize());
      this.observer.observe(canvas);
    }
    // A move to another monitor changes devicePixelRatio without changing the
    // CSS size; the window resize that comes with it is the cue to re-check.
    window.addEventListener("resize", this.onWindowResize);
    this.resize();
  }

  /** One of: idle | listening | thinking | success | error */
  setMode(mode: VisualizerMode): void {
    if (this.mode === mode) return;
    this.mode = mode;
    this.checkProgress = 0;
    if (mode === "listening") {
      // Clear the drawn heights too, not just the targets: otherwise the
      // first frames show the previous take's waveform decaying away.
      this.levels.fill(0);
      this.shown.fill(0);
    }
    this.draw();
  }

  /** Feed a new amplitude in 0..1 from the capture thread. */
  pushLevel(level: number): void {
    const clamped = Math.max(0, Math.min(1, Number.isFinite(level) ? level : 0));
    this.levels.shift();
    this.levels.push(clamped);
  }

  start(): void {
    if (this.timer) return;
    this.timer = window.setInterval(() => {
      this.advance();
      this.draw();
    }, FRAME_MS);
  }

  stop(): void {
    if (this.timer) {
      window.clearInterval(this.timer);
      this.timer = 0;
    }
  }

  destroy(): void {
    this.stop();
    this.observer?.disconnect();
    this.observer = null;
    window.removeEventListener("resize", this.onWindowResize);
  }

  private onWindowResize = (): void => {
    this.width = -1; // force the backing store to be re-checked against the DPR
    this.resize();
  };

  /** Track the element's CSS size; the backing store follows devicePixelRatio. */
  private resize(): void {
    const rect = this.canvas.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    const w = Math.round(rect.width);
    const h = Math.round(rect.height);
    if (w === this.width && h === this.height && this.canvas.width === Math.round(w * dpr)) return;
    this.width = w;
    this.height = h;
    this.canvas.width = Math.max(1, Math.round(w * dpr));
    this.canvas.height = Math.max(1, Math.round(h * dpr));
    this.draw();
  }

  private advance(): void {
    this.phase += FRAME_MS / 1000;
    if (this.mode === "success" || this.mode === "error") {
      this.checkProgress = Math.min(1, this.checkProgress + 0.06);
    }

    // Idle breathes on a slow sine so the pill never looks frozen.
    if (this.mode === "idle" || this.mode === "thinking") {
      for (let i = 0; i < BAR_COUNT; i++) {
        this.levels[i] = 0.1 + 0.05 * Math.sin(this.phase * 2.2 - i * 0.35);
      }
    }

    for (let i = 0; i < BAR_COUNT; i++) {
      const target = this.levels[i] ?? 0;
      const current = this.shown[i] ?? 0;
      const rate = target > current ? ATTACK : RELEASE;
      this.shown[i] = current + (target - current) * rate;
    }
  }

  private draw(): void {
    const { width, height } = this;
    if (width <= 0 || height <= 0) return;

    const cr = this.ctx;
    const dpr = window.devicePixelRatio || 1;
    cr.setTransform(dpr, 0, 0, dpr, 0, 0);
    cr.clearRect(0, 0, width, height);
    cr.lineCap = "round";
    cr.lineWidth = BAR_WIDTH;

    switch (this.mode) {
      case "thinking":
        this.drawDots(cr, width, height);
        break;
      case "success":
        this.drawCheck(cr, width, height, PALETTE.success);
        break;
      case "error":
        this.drawCross(cr, width, height, PALETTE.danger);
        break;
      default:
        this.drawBars(cr, width, height);
        break;
    }
  }

  private drawBars(cr: CanvasRenderingContext2D, width: number, height: number): void {
    const colour = this.mode === "listening" ? PALETTE.accent : PALETTE.muted;
    const mid = height / 2;
    const maxHalf = height / 2 - BAR_WIDTH;

    // The pill shrinks the visualizer when a transcript shares the row, so
    // fit the bar count to the width we actually got. Drawing a fixed 26
    // bars into 84px overlaps them into a wedge.
    const slot = BAR_WIDTH + BAR_GAP;
    const count = Math.max(4, Math.min(BAR_COUNT, Math.floor((width + BAR_GAP) / slot)));
    const offset = BAR_COUNT - count; // keep the most recent samples

    const span = count * BAR_WIDTH + (count - 1) * BAR_GAP;
    let x = (width - span) / 2 + BAR_WIDTH / 2;

    for (let i = 0; i < count; i++) {
      // Taper the ends so the waveform fades out rather than cutting off.
      const edge = Math.min(i, count - 1 - i) / (count / 4);
      const taper = Math.min(1, edge);
      const half = Math.max(BAR_WIDTH / 2, (this.shown[offset + i] ?? 0) * maxHalf * taper);

      cr.strokeStyle = withAlpha(colour, 0.35 + 0.65 * taper);
      cr.beginPath();
      cr.moveTo(x, mid - half);
      cr.lineTo(x, mid + half);
      cr.stroke();
      x += slot;
    }
  }

  private drawDots(cr: CanvasRenderingContext2D, width: number, height: number): void {
    const count = 3;
    const radius = 3.5;
    const gap = 12;
    const span = (count - 1) * gap;
    const mid = height / 2;
    let x = (width - span) / 2;

    for (let i = 0; i < count; i++) {
      // Each dot rides the same sine, offset, so they chase one another.
      const wave = Math.sin(this.phase * 4.5 - i * 0.9);
      const scale = 0.65 + (0.35 * (wave + 1)) / 2;
      cr.fillStyle = withAlpha(PALETTE.thinking, 0.45 + (0.55 * (wave + 1)) / 2);
      cr.beginPath();
      cr.arc(x, mid, radius * scale, 0, 2 * Math.PI);
      cr.fill();
      x += gap;
    }
  }

  private drawCheck(cr: CanvasRenderingContext2D, width: number, height: number, colour: string): void {
    const t = this.checkProgress;
    const cx = width / 2;
    const cy = height / 2;
    const s = Math.min(width, height) * 0.22;

    // Two strokes drawn in sequence: short down-leg, then the long up-leg.
    const p0 = [cx - s, cy] as const;
    const p1 = [cx - s * 0.25, cy + s * 0.7] as const;
    const p2 = [cx + s, cy - s * 0.65] as const;

    cr.strokeStyle = withAlpha(colour, 1);
    cr.lineWidth = 3.5;
    cr.beginPath();
    cr.moveTo(p0[0], p0[1]);

    const legOne = Math.min(1, t / 0.4);
    cr.lineTo(p0[0] + (p1[0] - p0[0]) * legOne, p0[1] + (p1[1] - p0[1]) * legOne);

    if (t > 0.4) {
      const legTwo = Math.min(1, (t - 0.4) / 0.6);
      cr.lineTo(p1[0] + (p2[0] - p1[0]) * legTwo, p1[1] + (p2[1] - p1[1]) * legTwo);
    }
    cr.stroke();
  }

  private drawCross(cr: CanvasRenderingContext2D, width: number, height: number, colour: string): void {
    const t = this.checkProgress;
    const cx = width / 2;
    const cy = height / 2;
    const s = Math.min(width, height) * 0.18;

    cr.strokeStyle = withAlpha(colour, 1);
    cr.lineWidth = 3.5;

    const a = Math.min(1, t / 0.5);
    cr.beginPath();
    cr.moveTo(cx - s, cy - s);
    cr.lineTo(cx - s + 2 * s * a, cy - s + 2 * s * a);
    cr.stroke();

    if (t > 0.5) {
      const b = Math.min(1, (t - 0.5) / 0.5);
      cr.beginPath();
      cr.moveTo(cx + s, cy - s);
      cr.lineTo(cx + s - 2 * s * b, cy - s + 2 * s * b);
      cr.stroke();
    }
  }
}
