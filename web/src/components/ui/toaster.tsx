import { useEffect, useState } from "react"
import { Toaster as ToasterPrimitive, toast as toastPrimitive, type ToastOptions } from "@/components/affect/toaster-clean"
import { useT } from "@/i18n"

/* Steve's toasts: affectUI's Toaster, once at the root (App.tsx). Short notes, the newest in front, centred
 * over and just above what sits at the bottom of the window – the composer on a chat or an item, the tab bar on a phone –
 * so a toast never covers the Send or Stop button it follows from. While a dialog or a sheet is open, that panel is the
 * only thing that counts: the toast goes above it, below it, beside it, or at the top of the window when neither has room, so an
 * error from a dialog's Save never covers the dialog's fields. */

/** What a toast must stay above: the composer (affectUI Chat Composer) with the label and bars over it, and a phone's
 * tab bar */
const OBSTACLES = ".ui-ccc, [data-item-feedback-label], [data-composer-bars], nav.fixed.bottom-0"
/** An open dialog's card or a sheet (ui/dialog.tsx, alert-dialog.tsx, sheet.tsx), not one on its way out */
const PANELS = ".ui-dlgc:not([data-ending-style]) > .ui-dlgc__panel, [data-slot=sheet-content]:not([data-ending-style])"
/** What moves the composer's column without resizing it: the sidebar folding (⌘B) */
const COLUMN = "[data-slot=sidebar-inset]"
const GAP = 12
const EDGE = 20
/** Room a toast needs above a panel (one toast's height and the gaps) */
const ROOM = 96

let measure: (() => void) | null = null
let setEdge: ((y: "top" | "bottom") => void) | null = null

/** The toast's width as affectUI draws it (--ui-tsc-width, 356px) */
function toastWidth() {
  const el = document.querySelector<HTMLElement>(".ui-tsc.steve-toaster")
  return (el && parseFloat(getComputedStyle(el).getPropertyValue("--ui-tsc-width"))) || 356
}

/** Where the toast goes. With a dialog or sheet open: above its panel, centred on it; else below it (a sheet from the top); else in the free space beside it
 * (a side sheet on a wide window); else at the top of the window. Otherwise centred over the lowest obstacle (the
 * composer's column) and just above it; without one, centred at the window's bottom edge. */
function place() {
  const h = window.innerHeight, w = window.innerWidth
  const root = document.documentElement.style
  const at = (y: "top" | "bottom", bottom: number, mid: number) => {
    root.setProperty("--steve-toast-bottom", `${Math.round(bottom)}px`)
    root.setProperty("--steve-toast-x", `${Math.round(mid)}px`)
    setEdge?.(y)
  }
  const panels = [...document.querySelectorAll<HTMLElement>(PANELS)]
  const panel = panels[panels.length - 1]
  if (panel) {
    const r = panel.getBoundingClientRect()
    if (r.top >= ROOM) return at("bottom", h - r.top + GAP, r.left + r.width / 2)
    if (h - r.bottom >= ROOM) return at("bottom", EDGE, r.left + r.width / 2)
    const tw = toastWidth() + 2 * EDGE
    if (w - r.right >= tw) return at("bottom", EDGE, (r.right + w) / 2)
    if (r.left >= tw) return at("bottom", EDGE, r.left / 2)
    return at("top", EDGE, r.left + r.width / 2)
  }
  let top = h, mid = w / 2
  for (const el of document.querySelectorAll<HTMLElement>(OBSTACLES)) {
    const r = el.getBoundingClientRect()
    if (!r.width || !r.height || r.bottom < h * 0.5 || r.top > h) continue
    if (r.top < top) { top = r.top; if (r.width < w - 1) mid = r.left + r.width / 2 }
  }
  at("bottom", top < h ? h - top + GAP : EDGE, mid)
}

/** In development: a Base UI popup opening while an affectUI Popover is up draws beneath it (ui/popover.tsx) */
function warnStacking(added: Node) {
  if (!(added instanceof HTMLElement)) return
  try { if (!document.querySelector(":popover-open")) return } catch { return }
  if (added.querySelector("[data-slot=dropdown-menu-content], [data-slot=select-content], .ui-dlgc"))
    console.warn("A Base UI menu, select or dialog opened inside a Popover: it draws beneath the Popover's top layer")
}

export function Toaster() {
  const t = useT()
  const [y, setY] = useState<"top" | "bottom">("bottom")
  useEffect(() => {
    let ro: ResizeObserver | null = null
    let frame = 0
    const run = () => {
      place()
      // a composer that grows while a toast is up (a longer message, an attachment), a dialog's card that grows, or
      // the sidebar folding, moves the toast with it
      ro?.disconnect()
      ro = new ResizeObserver(() => place())
      document.querySelectorAll(`${OBSTACLES}, ${PANELS}, ${COLUMN}`).forEach((el) => ro!.observe(el))
    }
    const soon = () => { cancelAnimationFrame(frame); frame = requestAnimationFrame(run) }
    // a dialog or sheet opening or closing (Base UI portals them into the body), and again once its card has slid into place
    const mo = new MutationObserver((records) => {
      if (import.meta.env.DEV) for (const r of records) r.addedNodes.forEach(warnStacking)
      soon()
    })
    mo.observe(document.body, { childList: true })
    const settled = (e: Event) => { if ((e.target as Element).matches?.(PANELS)) soon() }
    measure = run
    setEdge = setY
    run()
    window.addEventListener("resize", run)
    window.addEventListener("hashchange", run)
    document.addEventListener("animationend", settled, true)
    document.addEventListener("transitionend", settled, true)
    return () => {
      measure = null; setEdge = null; ro?.disconnect(); mo.disconnect(); cancelAnimationFrame(frame)
      window.removeEventListener("resize", run); window.removeEventListener("hashchange", run)
      document.removeEventListener("animationend", settled, true)
      document.removeEventListener("transitionend", settled, true)
    }
  }, [])
  return <ToasterPrimitive className="steve-toaster" position={`${y}-center`} visibleToasts={3} duration={4000} hotkey="F8"
    label={t("core:toast.region")} closeLabel={t("core:toast.dismiss")} />
}

type Toast = typeof toastPrimitive
const before = <A extends unknown[], R>(f: (...a: A) => R) => (...a: A): R => { measure?.(); return f(...a) }

/** toast("Saved"), toast.success(…), toast.error(…), toast.promise(…), toast.dismiss(id?) – as sonner's, which it replaces */
export const toast: Toast = Object.assign(before(toastPrimitive), {
  success: before(toastPrimitive.success),
  error: before(toastPrimitive.error),
  info: before(toastPrimitive.info),
  loading: before(toastPrimitive.loading),
  promise: before(toastPrimitive.promise) as Toast["promise"],
  dismiss: toastPrimitive.dismiss,
})
export type { ToastOptions }
