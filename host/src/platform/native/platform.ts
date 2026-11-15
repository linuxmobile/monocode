export const IS_LINUX =
  typeof navigator !== "undefined" && /Linux/i.test(navigator.platform);

/** The host runs on Linux; the desktop draws its own frame. */
export const HAS_NATIVE_GLASS = IS_LINUX;

export const MOD = "Ctrl+";
export const ALT = "Alt+";
export const SHIFT = "Shift+";
