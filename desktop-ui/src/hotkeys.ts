// Hotkey combos are stored as `KeyboardEvent.code` with modifier prefixes,
// e.g. "Space", "KeyF", "Shift+KeyL". `code` names the physical key, so a
// binding recorded on one machine means the same key on any layout.

/** Build the combo string for a key event, or null for a bare modifier. */
export function comboFromEvent(e: KeyboardEvent): string | null {
  // Some synthetic/virtual keyboards emit events with an empty code.
  if (!e.code || /^(Control|Shift|Alt|Meta)/.test(e.code)) return null;
  let combo = "";
  if (e.ctrlKey) combo += "Ctrl+";
  if (e.altKey) combo += "Alt+";
  if (e.shiftKey) combo += "Shift+";
  if (e.metaKey) combo += "Meta+";
  return combo + e.code;
}

const isMac =
  typeof navigator !== "undefined" && /Mac/.test(navigator.platform);

const KEY_LABELS: Record<string, string> = {
  Space: "Space",
  Escape: "Esc",
  ArrowUp: "↑",
  ArrowDown: "↓",
  ArrowLeft: "←",
  ArrowRight: "→",
  Backslash: "\\",
  Slash: "/",
  Comma: ",",
  Period: ".",
  Semicolon: ";",
  Quote: "'",
  BracketLeft: "[",
  BracketRight: "]",
  Backquote: "`",
  Minus: "-",
  Equal: "=",
};

/** Human-readable label for a stored combo ("Shift+KeyL" → "Shift+L"). */
export function comboLabel(combo: string): string {
  return combo
    .split("+")
    .map((part) => {
      if (part === "Meta") return isMac ? "Cmd" : "Win";
      if (part in KEY_LABELS) return KEY_LABELS[part];
      const key = part.replace(/^Key/, "").replace(/^Digit/, "");
      return key;
    })
    .join("+");
}
