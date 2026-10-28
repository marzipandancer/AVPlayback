// Hotkey combos are stored as `KeyboardEvent.code` with modifier prefixes,
// e.g. "Space", "KeyF", "Shift+KeyL". `code` names the physical key, so a
// binding recorded on one machine means the same key on any layout.

import { useEffect } from "react";
import type { Snapshot } from "./store";
import type { Command, Hotkeys } from "./types";

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

// Inputs that hold a value rather than typed text.
const NON_TEXT_INPUTS = new Set([
  "checkbox",
  "radio",
  "range",
  "button",
  "submit",
  "reset",
  "color",
  "file",
]);

/**
 * True when a key belongs to the focused element because the operator is
 * typing: a text field, text area or editable text. Checkboxes, sliders,
 * dropdowns and buttons don't keep their keys. Windows' WebView2 focuses
 * them on click and the Mac's WebKit doesn't, so skipping them all left
 * every hotkey dead on Windows once a slider or loop box had been clicked.
 */
export function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.isContentEditable || target instanceof HTMLTextAreaElement) return true;
  return target instanceof HTMLInputElement && !NON_TEXT_INPUTS.has(target.type);
}

function commandFor(combo: string, hk: Hotkeys): Command | null {
  return combo === hk.take
    ? { type: "take", payload: { id: null } }
    : combo === hk.playPause
    ? { type: "playPause" }
    : combo === hk.kill
    ? { type: "kill" }
    : combo === hk.restart
    ? { type: "restart" }
    : combo === hk.prevClip
    ? { type: "selectDelta", payload: { delta: -1 } }
    : combo === hk.nextClip
    ? { type: "selectDelta", payload: { delta: 1 } }
    : combo === hk.jumpBack
    ? { type: "jump", payload: { deltaMs: -10000 } }
    : combo === hk.jumpFwd
    ? { type: "jump", payload: { deltaMs: 10000 } }
    : combo === hk.markIn
    ? { type: "markIn" }
    : combo === hk.markOut
    ? { type: "markOut" }
    : combo === hk.launchOutput
    ? { type: "launchOutput" }
    : combo === hk.toggleFullscreen
    ? { type: "toggleFullscreen" }
    : null;
}

/**
 * Configurable hotkeys (default Space = take, K = kill, P = pause…).
 * Ignored while typing in a field; a recording HotkeyRow swallows the event
 * in the capture phase before this bubble-phase handler runs. A handled key
 * has its default prevented, so Space can't also toggle a focused checkbox
 * or an arrow nudge a focused slider.
 */
export function useHotkeys(
  snap: Snapshot | null,
  send: (cmd: Command) => void,
  enabled = true
) {
  useEffect(() => {
    if (!snap || !enabled) return;
    const hk = snap.state.hotkeys;
    const handler = (e: KeyboardEvent) => {
      if (isTypingTarget(e.target)) return;
      const combo = comboFromEvent(e);
      if (!combo) return;
      const cmd = commandFor(combo, hk);
      if (cmd) {
        e.preventDefault();
        send(cmd);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [snap, send, enabled]);
}
