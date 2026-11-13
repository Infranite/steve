"use client"

import * as React from "react"
import { createPortal } from "react-dom"
import { mergeProps } from "@base-ui/react/merge-props"
import { useRender } from "@base-ui/react/use-render"
import { Popover as PopoverPrimitive } from "@/components/affect/popover-clean"
import { cn } from "cn"

/* Popover: affectUI's Popover (a card in the top layer beside its trigger; it flips to stay on screen, closes on a press
 * outside, Escape or tabbing away and hands the focus back) behind the Base UI-shaped parts Steve's call sites use:
 *   <Popover open onOpenChange><PopoverTrigger render={<Button … />}>…</PopoverTrigger><PopoverContent align="end">…</PopoverContent></Popover>
 * The card sits next to the trigger on screen. In the DOM it is moved out of the trigger's parents (into the dialog or
 * popover the trigger is in, else <body>): it is a fixed box in the top layer, so a parent's child selectors (a Field's
 * `*:w-full`, `[&>*]:…`) would otherwise size it against the window – that drew the model menu edge to edge.
 *
 * Stacking: the card is in the browser's top layer, above everything that uses z-index – Base UI's dialogs, menus and
 * selects (z-50) and the toaster. So do not open a Base UI popup (Select, DropdownMenu, Dialog) from inside a Popover:
 * it would draw beneath the card. In development a console warning says so when it happens (ui/toaster.tsx watches). */

type Side = "top" | "bottom" | "left" | "right" | "inline-start" | "inline-end"
type Ctx = { open: boolean; set: (open: boolean) => void; anchor: React.RefObject<HTMLElement | null> }
const PopoverCtx = React.createContext<Ctx | null>(null)

function Popover({ open: controlled, defaultOpen = false, onOpenChange, children }: {
  open?: boolean
  defaultOpen?: boolean
  onOpenChange?: (open: boolean) => void
  children?: React.ReactNode
}) {
  const [own, setOwn] = React.useState(defaultOpen)
  const open = controlled ?? own
  const anchor = React.useRef<HTMLElement | null>(null)
  const change = React.useRef(onOpenChange)
  change.current = onOpenChange
  const set = React.useCallback((v: boolean) => { setOwn(v); change.current?.(v) }, [])
  return <PopoverCtx.Provider value={{ open, set: (v) => v !== open && set(v), anchor }}>{children}</PopoverCtx.Provider>
}

function PopoverTrigger({ render, ref, nativeButton: _n, onClick, ...props }: useRender.ComponentProps<"button"> & { nativeButton?: boolean }) {
  const ctx = React.useContext(PopoverCtx)
  const own = React.useCallback((el: HTMLElement | null) => { if (ctx) ctx.anchor.current = el }, [ctx])
  const merged = React.useMemo(() => mergeRefs(own, ref), [own, ref])
  return useRender({
    defaultTagName: "button",
    render,
    ref: merged,
    props: mergeProps<"button">({
      "data-slot": "popover-trigger",
      "aria-haspopup": "dialog",
      "aria-expanded": ctx?.open ?? false,
      ...(ctx?.open ? { "data-popup-open": "" } : {}),
      onClick: (e: React.MouseEvent<HTMLButtonElement>) => { onClick?.(e); if (!e.defaultPrevented) ctx?.set(!ctx.open) },
    } as React.ComponentProps<"button">, props),
  })
}

function PopoverContent({
  className,
  align = "center",
  side = "bottom",
  sideOffset = 4,
  initialFocus,
  children,
  ...props
}: Omit<React.ComponentProps<"div">, "title"> & {
  align?: "start" | "center" | "end"
  side?: Side
  sideOffset?: number
  /** false leaves the focus on the trigger; otherwise the card's first focusable element takes it. (Unlike Base UI's
   * initialFocus it takes no element: the focus always goes back to the trigger on close.) */
  initialFocus?: boolean
}) {
  const ctx = React.useContext(PopoverCtx)
  if (!ctx) return null
  if (!ctx.open) return null
  const host = typeof document === "undefined" ? null : ctx.anchor.current?.closest<HTMLElement>('[popover], [role="dialog"], [role="alertdialog"]') ?? document.body
  const card = (
    <PopoverPrimitive
      {...props}
      open={ctx.open}
      onClose={() => ctx.set(false)}
      anchor={ctx.anchor}
      side={side === "inline-start" ? "left" : side === "inline-end" ? "right" : side}
      align={align}
      offset={sideOffset + 2}
      focusOnOpen={initialFocus !== false}
      data-slot="popover-content"
      data-open=""
      className={cn("flex max-w-[calc(100vw-16px)] flex-col gap-4", className)}
    >
      {children}
    </PopoverPrimitive>
  )
  return host ? createPortal(card, host) : card
}

function PopoverHeader({ className, ...props }: React.ComponentProps<"div">) {
  return <div data-slot="popover-header" className={cn("ui-popc__head flex flex-col gap-1", className)} {...props} />
}

function PopoverTitle({ className, ...props }: React.ComponentProps<"p">) {
  return <p data-slot="popover-title" className={cn("ui-popc__title", className)} {...props} />
}

function PopoverDescription({ className, ...props }: React.ComponentProps<"p">) {
  return <p data-slot="popover-description" className={cn("ui-popc__desc", className)} {...props} />
}

function mergeRefs<T>(...refs: (React.Ref<T> | undefined)[]) {
  return (el: T | null) => {
    for (const r of refs) {
      if (typeof r === "function") r(el)
      else if (r) (r as React.RefObject<T | null>).current = el
    }
  }
}

export {
  Popover,
  PopoverContent,
  PopoverDescription,
  PopoverHeader,
  PopoverTitle,
  PopoverTrigger,
}
