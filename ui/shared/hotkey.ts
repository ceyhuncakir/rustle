// Reading a key combination the way config.toml spells it: "Super+D",
// "Ctrl+Alt+Space". Modifiers first, in a fixed order, then one key.

const MODIFIER_KEYS = new Set(["Control", "Shift", "Alt", "Meta", "OS", "AltGraph", "Hyper", "Super"]);

/** KeyboardEvent.code -> the X11 keysym name config.toml expects. */
const NAMED_CODES: Record<string, string> = {
  Space: "Space",
  Enter: "Return",
  Escape: "Escape",
  Backspace: "BackSpace",
  Tab: "Tab",
  Delete: "Delete",
  Insert: "Insert",
  Home: "Home",
  End: "End",
  PageUp: "Page_Up",
  PageDown: "Page_Down",
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
  CapsLock: "Caps_Lock",
  ScrollLock: "Scroll_Lock",
  Pause: "Pause",
  PrintScreen: "Print",
  ContextMenu: "Menu",
  Minus: "minus",
  Equal: "equal",
  BracketLeft: "bracketleft",
  BracketRight: "bracketright",
  Semicolon: "semicolon",
  Quote: "apostrophe",
  Backquote: "grave",
  Backslash: "backslash",
  Comma: "comma",
  Period: "period",
  Slash: "slash",
};

export function modifiersOf(e: KeyboardEvent | React.KeyboardEvent): string[] {
  const mods: string[] = [];
  if (e.ctrlKey) mods.push("Ctrl");
  if (e.altKey) mods.push("Alt");
  if (e.shiftKey) mods.push("Shift");
  if (e.metaKey) mods.push("Super");
  return mods;
}

/** The non-modifier key name for a keyboard event, or null for a bare modifier. */
export function keyNameOf(e: KeyboardEvent | React.KeyboardEvent): string | null {
  if (MODIFIER_KEYS.has(e.key)) return null;
  const code = e.code;
  if (code.startsWith("Key") && code.length === 4) return code.slice(3);
  if (code.startsWith("Digit") && code.length === 6) return code.slice(5);
  if (/^F\d{1,2}$/.test(code)) return code;
  if (code.startsWith("Numpad")) return code;
  const named = NAMED_CODES[code];
  if (named) return named;
  if (e.key.length === 1) return e.key.toUpperCase();
  return e.key;
}

/** Split "Ctrl+Alt+Space" into its parts for display. */
export function partsOf(combo: string): string[] {
  return combo
    .split("+")
    .map((p) => p.trim())
    .filter(Boolean);
}

export function comboOf(modifiers: string[], key: string): string {
  return [...modifiers, key].join("+");
}

/** Whether a combination is usable as a global shortcut. */
export function isUsableCombo(modifiers: string[], key: string | null): key is string {
  if (!key) return false;
  if (/^F\d{1,2}$/.test(key)) return true;
  return modifiers.length > 0;
}
