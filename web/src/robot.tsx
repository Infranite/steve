/* Bot faces — Agent Robot Avatar (agent-robot-avatar, MIT). Each bot sets its own colour, skin and head shape.
 * Small faces are static: one hidden live instance renders each look once, its shadow DOM is snapshotted, and every avatar
 * on the page is a plain shadow root with that snapshot — no animation loop, no pointer tracking.
 * `live` faces (the hero) are the real component: they follow the pointer and react to the cue bus. */
import { useEffect, useRef } from "react"
import { cn } from "cn"
import { st, defaultColor } from "@/store"
import { onCue, type HeroCue } from "@/bus"

export { AVATAR_COLORS } from "@/store"
export type Look = { color: string; skin?: string | null; round?: number }
const keyOf = (l: Look) => `${l.color}|${l.skin ?? ""}|${l.round ?? 60}`
const cache = new Map<string, Promise<string>>()
let lib: Promise<typeof import("agent-robot-avatar")> | null = null
const frames = (n: number) => new Promise<void>((r) => { const f = () => (n-- <= 0 ? r() : requestAnimationFrame(f)); requestAnimationFrame(f) })

function snapshot(l: Look): Promise<string> {
  const k = keyOf(l)
  if (!cache.has(k)) cache.set(k, (async () => {
    const m = await (lib ??= import("agent-robot-avatar"))
    if (l.skin) await m.loadSkins()
    const el = document.createElement("agent-robot-avatar") as any
    for (const [a, v] of Object.entries({ size: "240", color: l.color, motion: "reduce", "auto-sleep": "0", "wake-on": "manual" })) el.setAttribute(a, v)
    if (l.skin) el.setAttribute("skin", l.skin)
    /* inside the viewport but invisible: the component pauses its frame loop when it is scrolled out of view */
    el.style.cssText = "position:fixed;left:0;top:0;opacity:0;pointer-events:none;z-index:-1"
    document.body.appendChild(el)
    try {
      await customElements.whenDefined("agent-robot-avatar")
      el.setPointerFollow?.(false); el.setHeadRoundness?.(l.round ?? 60); el.reset?.()
      /* the antenna and the final head outline are added by the first animation frames: wait for them */
      for (let n = 0; n < 60 && !el.shadowRoot?.getElementById("antennaDot"); n++) await frames(1)
      await frames(2)
      if (l.skin) await new Promise((r) => setTimeout(r, 150))
      return String(el.shadowRoot?.innerHTML ?? "")
    } finally { el.remove() }
  })())
  return cache.get(k)!
}

/** a static face: a shadow root holding a snapshot of the live component */
export function Face({ look, size, className }: { look: Look; size: number; className?: string }) {
  const host = useRef<HTMLSpanElement>(null)
  useEffect(() => {
    let live = true
    void snapshot(look).then((html) => {
      const h = host.current; if (!live || !h) return
      const root = h.shadowRoot ?? h.attachShadow({ mode: "open" })
      /* a picture, not a widget: no pointer events reach the copied svg */
      root.innerHTML = html.replace(/cursor:pointer;?/g, "") + "<style>:host{pointer-events:none}</style>"
    }).catch(() => {})
    return () => { live = false }
  }, [look.color, look.skin, look.round])
  return <span ref={host} aria-hidden className={cn("inline-block shrink-0", className)} style={{ width: size, height: size, ["--face-size" as any]: size + "px" }} />
}

/** the live component: follows the pointer, plays whatever the app cues */
function LiveFace({ look, size, className }: { look: Look; size: number; className?: string }) {
  const host = useRef<HTMLSpanElement>(null), el = useRef<any>(null)
  useEffect(() => {
    let live = true
    void (async () => {
      const m = await (lib ??= import("agent-robot-avatar"))
      if (look.skin) await m.loadSkins()
      await customElements.whenDefined("agent-robot-avatar")
      const h = host.current; if (!live || !h) return
      const a = document.createElement("agent-robot-avatar") as any
      for (const [k, v] of Object.entries({ size: String(size), color: look.color, motion: "auto", "auto-sleep": "90000", "wake-on": "activity" })) a.setAttribute(k, v)
      if (look.skin) a.setAttribute("skin", look.skin)
      h.replaceChildren(a); el.current = a
      a.setHeadRoundness?.(look.round ?? 60); a.setPointerFollow?.(true)
    })().catch(() => {})
    return () => { live = false; el.current?.remove(); el.current = null }
  }, [look.color, look.skin, look.round, size])
  useEffect(() => onCue((c: HeroCue) => {
    const a = el.current; if (!a) return
    a.noteActivity?.(true)
    if (c === "think") { a.input?.(false); void a.startWaiting?.(); return }
    a.stopWaiting?.()
    if (c === "input") { void a.input?.(true); return }
    a.input?.(false)
    if (c === "idle") { a.reset?.(); return }
    void a.play?.(c === "reply" ? "affirm" : c)
  }), [])
  return <span ref={host} aria-hidden className={cn("inline-flex shrink-0", className)} style={{ width: size, height: size }} />
}

export const lookOf = (k: string): Look => {
  const b = st.bots[k]
  return b ? { color: b.color, skin: b.skin, round: b.round } : { color: defaultColor(k), round: 60 }
}
export function Robot({ k, size = 24, live, className, color, skin, round }: { k: string; size?: number; live?: boolean; className?: string; color?: string; skin?: string | null; round?: number; shape?: string; seed?: string; name?: string }) {
  const base = lookOf(k)
  const look = { color: color ?? base.color, skin: skin !== undefined ? skin : base.skin, round: round ?? base.round }
  return live ? <LiveFace look={look} size={size} className={className} /> : <Face look={look} size={size} className={className} />
}
/** message tint for a bot — a whisper of its colour */
export const tintOf = (k: string) => `color-mix(in srgb, ${lookOf(k).color} ${document.documentElement.classList.contains("dark") ? 14 : 12}%, var(--background))`
