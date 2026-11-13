"use client"

import * as React from "react"
import { Dialog as DialogPrimitive } from "@base-ui/react/dialog"
import { cn } from "cn"
import { XIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { dialogCss } from "@/components/affect/dialog-clean"
import { useT } from "@/i18n"

/* Dialog: affectUI's Dialog look (clean) on Base UI's dialog. The layer is affectUI's – the dimmed window with the card
 * in the middle, a sheet from the bottom on a phone, its motion, title, quiet line and close × – while Base UI keeps the
 * focus, Escape, outside press and the stacking with the menus and selects that open from inside (affectUI's own
 * Dialog is a native <dialog>, which makes everything outside it inert, those popups included).
 * Viewport = .ui-dlgc (the layer), Popup = .ui-dlgc__panel (the card); styles in affect/dialog-clean.tsx and
 * styles/affectui-overlays.css.
 * Only dialog-clean.tsx's `dialogCss` export is used (by this file, alert-dialog.tsx and sheet.tsx); its Dialog and
 * AlertDialog components are not rendered anywhere and the bundle drops them. The file stays whole, as
 * scripts/affectui-pull.py writes it, so pulling it again updates the look in one step. */

/** affectUI Dialog's styles, once in <head> however many dialogs are open */
function DialogStyles() {
  return <style href="ui-dialog-clean" precedence="default">{dialogCss}</style>
}

function Dialog({ ...props }: DialogPrimitive.Root.Props) {
  return <DialogPrimitive.Root data-slot="dialog" {...props} />
}

function DialogTrigger({ ...props }: DialogPrimitive.Trigger.Props) {
  return <DialogPrimitive.Trigger data-slot="dialog-trigger" {...props} />
}

function DialogPortal({ ...props }: DialogPrimitive.Portal.Props) {
  return <DialogPrimitive.Portal data-slot="dialog-portal" {...props} />
}

function DialogClose({ ...props }: DialogPrimitive.Close.Props) {
  return <DialogPrimitive.Close data-slot="dialog-close" {...props} />
}

/** Not used by DialogContent, which dims the window with its viewport (.ui-dlgc); kept for a caller that wants a backdrop of its own */
function DialogOverlay({ className, ...props }: DialogPrimitive.Backdrop.Props) {
  return <DialogPrimitive.Backdrop data-slot="dialog-overlay" className={cn("fixed inset-0 z-50", className)} {...props} />
}

/** The close × in the card's corner, on the title's line */
function CloseX({ part }: { part: "dialog" | "sheet" }) {
  const t = useT()
  const label = t("common:action.close")
  return (
    <DialogPrimitive.Close data-slot={`${part}-close`} aria-label={label} title={label}
      className="ui-dlgc__close absolute! top-[19px] right-4 m-0! [&_svg]:size-[15px]">
      <XIcon strokeWidth={1.9} />
    </DialogPrimitive.Close>
  )
}

function DialogContent({
  className,
  children,
  showCloseButton = true,
  ...props
}: DialogPrimitive.Popup.Props & {
  showCloseButton?: boolean
}) {
  /* a dialog taller than the window scrolls inside instead of running off both edges */
  return (
    <DialogPortal>
      <DialogStyles />
      <DialogPrimitive.Viewport data-slot="dialog-viewport" className="ui-dlgc">
        <DialogPrimitive.Popup
          data-slot="dialog-content"
          className={cn(
            "ui-dlgc__panel grid w-full min-w-0 grid-cols-1 content-start gap-6 overflow-y-auto overscroll-contain p-6 outline-none sm:max-w-md",
            className
          )}
          {...props}
        >
          {children}
          {showCloseButton && <CloseX part="dialog" />}
        </DialogPrimitive.Popup>
      </DialogPrimitive.Viewport>
    </DialogPortal>
  )
}

function DialogHeader({ className, ...props }: React.ComponentProps<"div">) {
  return (
    <div
      data-slot="dialog-header"
      className={cn("flex flex-col gap-1 pr-8", className)}
      {...props}
    />
  )
}

function DialogFooter({
  className,
  showCloseButton = false,
  children,
  ...props
}: React.ComponentProps<"div"> & {
  showCloseButton?: boolean
}) {
  const t = useT()
  return (
    <div
      data-slot="dialog-footer"
      className={cn(
        "flex flex-col-reverse gap-2 sm:flex-row sm:flex-wrap sm:justify-end",
        className
      )}
      {...props}
    >
      {children}
      {showCloseButton && (
        <DialogPrimitive.Close render={<Button variant="outline" />}>{t("common:action.close")}</DialogPrimitive.Close>
      )}
    </div>
  )
}

function DialogTitle({ className, ...props }: DialogPrimitive.Title.Props) {
  return (
    <DialogPrimitive.Title
      data-slot="dialog-title"
      className={cn("ui-dlgc__title", className)}
      {...props}
    />
  )
}

function DialogDescription({
  className,
  ...props
}: DialogPrimitive.Description.Props) {
  return (
    <DialogPrimitive.Description
      data-slot="dialog-description"
      className={cn(
        "ui-dlgc__desc *:[a]:underline *:[a]:underline-offset-3 *:[a]:hover:text-foreground",
        className
      )}
      {...props}
    />
  )
}

export {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogOverlay,
  DialogPortal,
  DialogTitle,
  DialogTrigger,
  DialogStyles,
  CloseX,
}
