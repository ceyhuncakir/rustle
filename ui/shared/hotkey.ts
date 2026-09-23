// Reading a key combination the way config.toml and the global-shortcut
// plugin spell it: "Super+D", "Ctrl+Alt+Space". Modifiers first, in a fixed
// order, then one key. Every name produced here is one the Rust side parses
// (global-hotkey's `parse_key`, which mostly takes KeyboardEvent.code names);
// how a combination is shown is a separate matter, see `keyLabel`.

const MODIFIER_KEYS = new Set(["Control", "Shift", "Alt", "Meta", "OS", "AltGraph", "Hyper", "Super"]);

/** KeyboardEvent.code -> the parser's name, for everything but letters, digits and F-keys. */
const NAMED_CODES: Record<string, string> = {
  Space: "Space",
  Enter: "Enter",
  Backspace: "Backspace",
  Tab: "Tab",
  Delete: "Delete",
  Insert: "Insert",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
  CapsLock: "CapsLock",
  NumLock: "NumLock",
  ScrollLock: "ScrollLock",
  Pause: "Pause",
  PrintScreen: "PrintScreen",
  Minus: "Minus",
  Equal: "Equal",
  BracketLeft: "BracketLeft",
  BracketRight: "BracketRight",
  Semicolon: "Semicolon",
  Quote: "Quote",
  Backquote: "Backquote",
  Backslash: "Backslash",
  Comma: "Comma",
  Period: "Period",
  Slash: "Slash",
  Numpad0: "Numpad0",
  Numpad1: "Numpad1",
  Numpad2: "Numpad2",
  Numpad3: "Numpad3",
  Numpad4: "Numpad4",
  Numpad5: "Numpad5",
  Numpad6: "Numpad6",
  Numpad7: "Numpad7",
  Numpad8: "Numpad8",
  Numpad9: "Numpad9",
  NumpadAdd: "NumpadAdd",
  NumpadDecimal: "NumpadDecimal",
  NumpadDivide: "NumpadDivide",
  NumpadEnter: "NumpadEnter",
  NumpadEqual: "NumpadEqual",
  NumpadMultiply: "NumpadMultiply",
  NumpadSubtract: "NumpadSubtract",
};

/** F1 to F24, the parser's whole range. */
const FUNCTION_KEY = /^F([1-9]|1\d|2[0-4])$/;

const platform = typeof navigator === "undefined" ? "" : navigator.userAgent;
export const IS_MAC = /Macintosh|Mac OS X/.test(platform);
const IS_WINDOWS = /Windows/.test(platform);

export function modifiersOf(e: KeyboardEvent | React.KeyboardEvent): string[] {
  const mods: string[] = [];
  if (e.ctrlKey) mods.push("Ctrl");
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey) mods.push("Shift");
  if (e.metaKey) mods.push("Super");
  return mods;
}

/** Whether the event is a modifier going down on its own. */
export function isModifierOnly(e: KeyboardEvent | React.KeyboardEvent): boolean {
  return MODIFIER_KEYS.has(e.key);
}

/**
 * The shortcut name of the event's key, from its physical position, or null
 * for a bare modifier and for keys a global shortcut cannot use (Menu, media
 * keys, the extra keys of non-US layouts).
 */
export function keyNameOf(e: KeyboardEvent | React.KeyboardEvent): string | null {
  if (isModifierOnly(e)) return null;
  const code = e.code;
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit\d$/.test(code)) return code.slice(5);
  if (FUNCTION_KEY.test(code)) return code;
  return NAMED_CODES[code] ?? null;
}

/** Split "Ctrl+Alt+Space" into its parts. */
export function partsOf(combo: string): string[] {
  return combo
    .split("+")
    .map((p) => p.trim())
    .filter(Boolean);
}

export function comboOf(modifiers: string[], key: string): string {
  return [...modifiers, key].join("+");
}

/**
 * Whether a combination is usable as a global shortcut: an F-key alone, or
 * anything with Ctrl, Alt or Super. Shift alone would take a character away
 * from typing everywhere.
 */
export function isUsableCombo(modifiers: string[], key: string | null): key is string {
  if (!key) return false;
  if (FUNCTION_KEY.test(key)) return true;
  return modifiers.some((m) => m !== "Shift");
}

// -- display -------------------------------------------------------------------

/** Keycap text, by the lower-cased name. macOS shows its own symbols. */
const LABELS: Record<string, string> = {
  ...(IS_MAC
    ? { super: "⌘", cmd: "⌘", command: "⌘", alt: "⌥", option: "⌥", ctrl: "⌃", control: "⌃", shift: "⇧" }
    : { super: IS_WINDOWS ? "Win" : "Super", cmd: "Super", command: "Super", option: "Alt", control: "Ctrl" }),
  minus: "-",
  equal: "=",
  bracketleft: "[",
  bracketright: "]",
  semicolon: ";",
  quote: "'",
  backquote: "`",
  backslash: "\\",
  comma: ",",
  period: ".",
  slash: "/",
  up: "↑",
  down: "↓",
  left: "←",
  right: "→",
  arrowup: "↑",
  arrowdown: "↓",
  arrowleft: "←",
  arrowright: "→",
};

/** What a screen reader says for the modifiers macOS draws as symbols. */
const SPOKEN: Record<string, string> = IS_MAC
  ? { super: "Command", cmd: "Command", command: "Command", alt: "Option", option: "Option", ctrl: "Control", shift: "Shift" }
  : {};

/** How one part of a combination is shown on its keycap. */
export function keyLabel(part: string): string {
  const lower = part.toLowerCase();
  if (LABELS[lower]) return LABELS[lower];
  if (lower.startsWith("numpad")) return `Num ${part.slice(6)}`;
  return part.length === 1 ? part.toUpperCase() : part;
}

/** A combination as plain text: "Ctrl+Alt+Space", or "⌥D" on macOS. */
export function displayCombo(combo: string): string {
  return partsOf(combo).map(keyLabel).join(IS_MAC ? "" : "+");
}

/** A combination as a screen reader should say it. */
export function spokenCombo(combo: string): string {
  return partsOf(combo)
    .map((p) => SPOKEN[p.toLowerCase()] ?? p)
    .join(" ");
}
