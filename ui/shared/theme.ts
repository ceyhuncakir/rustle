// Palette, geometry and timing for the island, ported one-to-one from
// extension/theme.js. The overlay pill and the wizard's level meter both
// paint from these so they cannot drift apart.

export const PALETTE = {
  accent: "#7AA3F7", // listening  - periwinkle   (0.48, 0.64, 0.97)
  thinking: "#A88CFA", // thinking   - violet       (0.66, 0.55, 0.98)
  success: "#6BD999", // inserted   - mint         (0.42, 0.85, 0.60)
  danger: "#F5737A", // error      - coral        (0.96, 0.45, 0.48)
  muted: "#9EA6B8", // idle bars                 (0.62, 0.65, 0.72)
} as const;

export type IslandState = "hidden" | "idle" | "listening" | "thinking" | "inserting" | "error";
export type VisualizerMode = "idle" | "listening" | "thinking" | "success" | "error";

export const STATES: readonly IslandState[] = ["hidden", "idle", "listening", "thinking", "inserting", "error"];

export const GEOMETRY = {
  height: 44, // collapsed pill height
  heightExpanded: 66, // with a transcript line
  bottomMargin: 56, // gap from the bottom of the work area (the OS window's job now)
  widths: {
    idle: 132,
    listening: 268,
    thinking: 176,
    inserting: 196,
    error: 300,
  } as Record<string, number>,
  /** Width the visualizer keeps when a transcript shares the pill with it. */
  visWidthWithText: 84,
  textPadding: 34,
  labelMaxWidth: 560,
  labelPaddingLeft: 12,
} as const;

export const TIMING = {
  morph: 320, // pill width/height morph
  fade: 180,
  settle: 240,
  autoHide: 1400, // how long the success state lingers
} as const;

export const EASING = {
  outExpo: "cubic-bezier(0.16, 1, 0.3, 1)",
  outBack: "cubic-bezier(0.34, 1.56, 0.64, 1)",
  outQuad: "ease-out",
  inQuad: "ease-in",
} as const;

export function modeForState(state: IslandState): VisualizerMode {
  if (state === "inserting") return "success";
  if (state === "hidden") return "idle";
  return state;
}
