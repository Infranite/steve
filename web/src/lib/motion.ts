/** Appearance choices from Settings → 外观 that apply app-wide: the type size (标准 / 大) and reduced motion (开 / 关).
 *  They live in the Hub's settings; this device keeps the last ones it saw so the first paint already uses them.
 *  Each one is a class on <html> that index.css reads (`font-large` scales the type scale through --fs,
 *  `reduce-motion` mirrors prefers-reduced-motion); code that animates from script asks `reducedMotion()`. */
import { WORKSPACE_SETTING_VALUES } from "@/gen"

const KEY = "steve-look"
const [FONT_NORMAL, FONT_LARGE] = WORKSPACE_SETTING_VALUES.fontSize
const [MOTION_ON, MOTION_OFF] = WORKSPACE_SETTING_VALUES.reduceMotion
const root = () => (typeof document !== "undefined" ? document.documentElement : null)

/** true when the system asks for less motion or the reduced-motion setting is on */
export function reducedMotion(): boolean {
  if (root()?.classList.contains("reduce-motion")) return true
  return typeof window !== "undefined" && !!window.matchMedia?.("(prefers-reduced-motion: reduce)").matches
}

export type Look = { fontSize?: string; reduceMotion?: string }
/** put the classes on <html> for these settings, and remember them on this device */
export function applyLook(s: Look) {
  const el = root(); if (!el) return
  const large = s.fontSize === FONT_LARGE, calm = s.reduceMotion === MOTION_ON
  el.classList.toggle("font-large", large)
  el.classList.toggle("reduce-motion", calm)
  try { localStorage.setItem(KEY, JSON.stringify({ fontSize: large ? FONT_LARGE : FONT_NORMAL, reduceMotion: calm ? MOTION_ON : MOTION_OFF })) } catch { /* private mode */ }
}
/** the choices this device saw last, before the Hub has answered */
export function applyStoredLook() {
  try { const v = localStorage.getItem(KEY); if (v) applyLook(JSON.parse(v) as Look) } catch { /* nothing stored */ }
}
