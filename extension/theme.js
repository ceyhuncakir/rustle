// Central palette + geometry for the Flow island.
//
// Colours live here as plain JS rather than in CSS custom properties because
// the visualizer paints with Cairo, and St's colour objects changed
// representation between GNOME releases (Clutter.Color -> Cogl.Color, 0-255
// ints -> 0-1 floats). Keeping one source of truth in JS avoids that whole
// class of bug. The stylesheet mirrors these values for the St widgets.

export const PALETTE = {
    accent:   [0.48, 0.64, 0.97],  // listening  - periwinkle
    thinking: [0.66, 0.55, 0.98],  // thinking   - violet
    success:  [0.42, 0.85, 0.60],  // inserted   - mint
    danger:   [0.96, 0.45, 0.48],  // error      - coral
    muted:    [0.62, 0.65, 0.72],  // idle bars
};

export const GEOMETRY = {
    height: 44,          // collapsed pill height
    heightExpanded: 66,  // with a transcript line
    bottomMargin: 56,    // gap from the bottom of the work area
    widths: {
        idle: 132,
        listening: 268,
        thinking: 176,
        inserting: 196,
        error: 300,
    },
};

export const TIMING = {
    morph: 320,      // pill width/height morph
    fade: 180,
    settle: 240,
    autoHide: 1400,  // how long the success state lingers
};

/** Apply a PALETTE entry to a Cairo context. */
export function setColor(cr, rgb, alpha = 1.0) {
    cr.setSourceRGBA(rgb[0], rgb[1], rgb[2], alpha);
}
