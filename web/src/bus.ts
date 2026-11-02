/* hero-avatar reaction bus: anything in the app can ask the big avatar to react */
export type HeroCue = "input" | "think" | "success" | "affirm" | "failure" | "wary" | "inspect" | "love" | "reply" | "idle"
const target = new EventTarget()
export const cue = (c: HeroCue) => target.dispatchEvent(new CustomEvent("cue", { detail: c }))
export const onCue = (f: (c: HeroCue) => void) => { const h = (e: Event) => f((e as CustomEvent).detail); target.addEventListener("cue", h); return () => target.removeEventListener("cue", h) }
