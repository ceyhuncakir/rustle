// The island: a floating pill that morphs between dictation states. A port
// of extension/island.js onto one DOM element and one canvas. The OS window
// is the Rust side's business; this only reports the size it wants.

import { EASING, GEOMETRY, STATES, TIMING, modeForState, type IslandState } from "../shared/theme";
import { Visualizer } from "../shared/visualizer";

const MORPH = `width ${TIMING.morph}ms ${EASING.outExpo}, height ${TIMING.morph}ms ${EASING.outExpo}`;
// Opacity and transform ease on different curves: the back curve overshoots
// on purpose, which is the pop that makes the pill feel physical, but an
// opacity overshoot flashes the pill transparent just as it should be solid.
const SETTLE = `opacity ${TIMING.settle}ms ${EASING.outQuad}, transform ${TIMING.settle}ms ${EASING.outBack}`;
const FADE = `opacity ${TIMING.fade}ms ${EASING.inQuad}, transform ${TIMING.fade}ms ${EASING.inQuad}`;

export class Island {
  readonly element: HTMLElement;
  private readonly label: HTMLElement;
  private readonly probe: HTMLElement;
  private readonly visualizer: Visualizer;

  private state: IslandState = "hidden";
  private text = "";
  private hideTimer = 0;
  private outTimer = 0;
  private lastTarget: [number, number] | null = null;

  /** Called whenever the pill wants a different window size. */
  onResize: ((width: number, height: number) => void) | null = null;

  constructor(element: HTMLElement) {
    this.element = element;
    const canvas = element.querySelector<HTMLCanvasElement>("canvas.visualizer");
    const label = element.querySelector<HTMLElement>(".label");
    if (!canvas || !label) throw new Error("island markup is missing the visualizer or label");
    this.label = label;
    this.visualizer = new Visualizer(canvas);

    this.probe = document.createElement("span");
    this.probe.className = "label label-probe";
    this.probe.setAttribute("aria-hidden", "true");
    document.body.appendChild(this.probe);

    element.style.width = `${GEOMETRY.widths.idle}px`;
    element.style.height = `${GEOMETRY.height}px`;
  }

  setState(state: IslandState): void {
    if (!STATES.includes(state)) throw new Error(`unknown island state: ${state}`);

    this.clearHideTimer();

    if (state === "hidden") {
      this.state = "hidden";
      this.animateOut();
      return;
    }

    const wasHidden = this.state === "hidden";
    this.state = state;

    this.visualizer.setMode(modeForState(state));

    if (wasHidden) {
      // animateIn already sizes the pill, so morphing to the same value only
      // adds a redundant transition.
      this.animateIn();
    } else {
      this.morph();
    }

    // The success state is a confirmation, not a mode - it clears itself.
    if (state === "inserting") {
      this.hideTimer = window.setTimeout(() => {
        this.hideTimer = 0;
        this.setState("hidden");
      }, TIMING.autoHide);
    }
  }

  setText(text: string | null | undefined): void {
    this.text = text ?? "";
    const hasText = this.text.length > 0;
    this.label.textContent = this.text;
    this.element.classList.toggle("has-text", hasText);
    if (this.state !== "hidden") this.morph();
  }

  pushLevel(level: number): void {
    this.visualizer.pushLevel(level);
  }

  /** Target pill geometry for the current state and text. */
  targetSize(): [number, number] {
    const base = GEOMETRY.widths[this.state] ?? GEOMETRY.widths.idle ?? 132;
    if (!this.text) return [base, GEOMETRY.height];

    const screenWidth = window.screen?.width || 0;
    const maxWidth = screenWidth > 0 ? Math.round(screenWidth * 0.55) : 720;
    const natural = this.labelNaturalWidth();
    const width = GEOMETRY.visWidthWithText + natural + GEOMETRY.textPadding;

    return [Math.max(base, Math.min(width, maxWidth)), GEOMETRY.heightExpanded];
  }

  // The label's preferred width, as St would report it: the text capped by
  // max-width, plus its left padding.
  private labelNaturalWidth(): number {
    this.probe.textContent = this.text;
    const measured = Math.ceil(this.probe.getBoundingClientRect().width);
    return Math.min(measured, GEOMETRY.labelMaxWidth) + GEOMETRY.labelPaddingLeft;
  }

  private applySize(width: number, height: number): void {
    this.element.style.width = `${width}px`;
    this.element.style.height = `${height}px`;
    if (this.lastTarget && this.lastTarget[0] === width && this.lastTarget[1] === height) return;
    this.lastTarget = [width, height];
    this.onResize?.(width, height);
  }

  private morph(): void {
    const [width, height] = this.targetSize();
    this.element.style.transition = `${MORPH}, ${SETTLE}`;
    this.applySize(width, height);
  }

  private animateIn(): void {
    this.visualizer.start();
    if (this.outTimer) {
      window.clearTimeout(this.outTimer);
      this.outTimer = 0;
    }

    const el = this.element;
    el.style.visibility = "visible";
    el.style.transition = "none";

    const [width, height] = this.targetSize();
    this.applySize(width, height);

    el.style.transform = "scale(0.88) translateY(18px)";
    el.style.opacity = "0";
    // Flush the start values so the transition has somewhere to start from.
    void el.offsetWidth;

    el.style.transition = `${MORPH}, ${SETTLE}`;
    el.style.transform = "scale(1) translateY(0)";
    el.style.opacity = "1";
  }

  private animateOut(): void {
    if (this.outTimer) window.clearTimeout(this.outTimer);
    const el = this.element;
    el.style.transition = FADE;
    el.style.opacity = "0";
    el.style.transform = "scale(0.9) translateY(14px)";

    this.outTimer = window.setTimeout(() => {
      this.outTimer = 0;
      el.style.visibility = "hidden";
      this.visualizer.stop();
      this.visualizer.setMode("idle");
      this.setText("");
    }, TIMING.fade);
  }

  private clearHideTimer(): void {
    if (this.hideTimer) {
      window.clearTimeout(this.hideTimer);
      this.hideTimer = 0;
    }
  }

  destroy(): void {
    this.clearHideTimer();
    if (this.outTimer) window.clearTimeout(this.outTimer);
    this.visualizer.destroy();
    this.probe.remove();
  }
}
