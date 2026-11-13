import * as React from "react"
import { Menu as MenuPrimitive } from "@base-ui/react/menu"
import { cn } from "cn"
import { ChevronRightIcon, CheckIcon } from "lucide-react"
import { dropdownMenuCss } from "@/components/affect/dropdown-menu-clean"

/* Dropdown Menu: affectUI's Dropdown Menu look (clean) on Base UI's menu – the card, the quiet rows that turn to ink
 * under the pointer or the arrow keys, one soft highlight gliding between them, red items, the small heading, the
 * dividers and checks. affectUI's own menu takes its items as data; Steve's call sites put free-form children in
 * (a search field, two-line rows, their data-* hooks), so Base UI keeps the behaviour – keyboard, type-ahead,
 * submenus, focus – and these parts carry affectUI's classes (.ui-dmc__*, styles in affect/dropdown-menu-clean.tsx).
 * Only dropdown-menu-clean.tsx's `dropdownMenuCss` export is used; its DropdownMenu component is not rendered anywhere
 * and the bundle drops it. The file stays whole, as scripts/affectui-pull.py writes it, so pulling it again updates
 * the look in one step. */

function DropdownMenu({ ...props }: MenuPrimitive.Root.Props) {
  return <MenuPrimitive.Root data-slot="dropdown-menu" {...props} />
}

function DropdownMenuPortal({ ...props }: MenuPrimitive.Portal.Props) {
  return <MenuPrimitive.Portal data-slot="dropdown-menu-portal" {...props} />
}

function DropdownMenuTrigger({ ...props }: MenuPrimitive.Trigger.Props) {
  return <MenuPrimitive.Trigger data-slot="dropdown-menu-trigger" {...props} />
}

/** affectUI's gliding highlight: one soft block that moves to the row Base UI highlights (pointer or keys). Set up
 * when the popup mounts (it only exists while the menu is open). */
function useGlow() {
  const [el, setEl] = React.useState<HTMLDivElement | null>(null)
  const glow = React.useRef<HTMLSpanElement | null>(null)
  React.useLayoutEffect(() => {
    const g = glow.current
    if (!el || !g) return
    let shown = false
    const move = () => {
      const at = [...el.querySelectorAll<HTMLElement>("[data-highlighted]")].find((x) => x.closest("[data-slot$='menu-content']") === el)
      if (!at) { g.style.opacity = "0"; shown = false; return }
      // measured without the popup's own opening scale
      const x = at.offsetLeft, y = at.offsetTop
      // the first highlight appears in place; after that it glides
      g.style.transition = shown ? "" : "none"
      g.style.transform = `translate(${x}px, ${y}px)`
      g.style.width = `${at.offsetWidth}px`
      g.style.height = `${at.offsetHeight}px`
      g.style.opacity = "1"
      g.toggleAttribute("data-danger", at.dataset.variant === "destructive")
      shown = true
    }
    const mo = new MutationObserver(move)
    mo.observe(el, { subtree: true, attributes: true, attributeFilter: ["data-highlighted"] })
    move()
    return () => mo.disconnect()
  }, [el])
  return { setEl, glow }
}

function DropdownMenuContent({
  align = "start",
  alignOffset = 0,
  side = "bottom",
  sideOffset = 6,
  className,
  children,
  ref,
  ...props
}: MenuPrimitive.Popup.Props &
  Pick<
    MenuPrimitive.Positioner.Props,
    "align" | "alignOffset" | "side" | "sideOffset"
  >) {
  const { setEl, glow } = useGlow()
  const setRef = React.useCallback((el: HTMLDivElement | null) => {
    setEl(el)
    if (typeof ref === "function") ref(el)
    else if (ref) (ref as React.RefObject<HTMLDivElement | null>).current = el
  }, [ref, setEl])
  return (
    <MenuPrimitive.Portal>
      <style href="ui-dropdown-menu-clean" precedence="default">{dropdownMenuCss}</style>
      <MenuPrimitive.Positioner
        className="isolate z-50 outline-none"
        align={align}
        alignOffset={alignOffset}
        side={side}
        sideOffset={sideOffset}
      >
        <MenuPrimitive.Popup
          ref={setRef}
          data-slot="dropdown-menu-content"
          className={cn("ui-dmc ui-dmc__panel relative z-50 block max-h-(--available-height) w-(--anchor-width) overflow-x-hidden overflow-y-auto outline-none", className)}
          {...props}
        >
          <span ref={glow} aria-hidden className="ui-dmc__glow right-auto opacity-0" />
          {children}
        </MenuPrimitive.Popup>
      </MenuPrimitive.Positioner>
    </MenuPrimitive.Portal>
  )
}

function DropdownMenuGroup({ ...props }: MenuPrimitive.Group.Props) {
  return <MenuPrimitive.Group data-slot="dropdown-menu-group" {...props} />
}

function DropdownMenuLabel({
  className,
  inset,
  ...props
}: MenuPrimitive.GroupLabel.Props & {
  inset?: boolean
}) {
  return (
    <MenuPrimitive.GroupLabel
      data-slot="dropdown-menu-label"
      data-inset={inset}
      className={cn("ui-dmc__title data-inset:pl-8", className)}
      {...props}
    />
  )
}

/** a row: affectUI's item, with Steve's icons (a 15px glyph in the quiet icon colour) */
const ROW = "ui-dmc__item relative gap-2.5 whitespace-normal [&_svg]:pointer-events-none [&_svg]:shrink-0 [&_svg:not([class*='size-'])]:size-[15px] [&>svg:not([class*='text-'])]:text-(--ui-dmc-icon) [&[data-highlighted]:not([data-danger])]:text-(--ui-dmc-ink) [&[data-highlighted]:not([data-danger])>svg:not([class*=text-])]:text-(--ui-dmc-ink) data-inset:pl-8"

function DropdownMenuItem({
  className,
  inset,
  variant = "default",
  disabled,
  ...props
}: MenuPrimitive.Item.Props & {
  inset?: boolean
  variant?: "default" | "destructive"
}) {
  return (
    <MenuPrimitive.Item
      data-slot="dropdown-menu-item"
      data-inset={inset}
      data-variant={variant}
      data-danger={variant === "destructive" ? "" : undefined}
      aria-disabled={disabled || undefined}
      disabled={disabled}
      className={cn(ROW, "group/dropdown-menu-item data-[variant=destructive]:*:[svg]:text-(--ui-dmc-danger)!", className)}
      {...props}
    />
  )
}

function DropdownMenuSub({ ...props }: MenuPrimitive.SubmenuRoot.Props) {
  return <MenuPrimitive.SubmenuRoot data-slot="dropdown-menu-sub" {...props} />
}

function DropdownMenuSubTrigger({
  className,
  inset,
  children,
  ...props
}: MenuPrimitive.SubmenuTrigger.Props & {
  inset?: boolean
}) {
  return (
    <MenuPrimitive.SubmenuTrigger
      data-slot="dropdown-menu-sub-trigger"
      data-inset={inset}
      className={cn(ROW, "data-popup-open:text-(--ui-dmc-ink)", className)}
      {...props}
    >
      {children}
      <span className="ui-dmc__more ml-auto"><ChevronRightIcon /></span>
    </MenuPrimitive.SubmenuTrigger>
  )
}

function DropdownMenuSubContent({
  align = "start",
  alignOffset = -4,
  side = "right",
  sideOffset = 2,
  className,
  ...props
}: React.ComponentProps<typeof DropdownMenuContent>) {
  return (
    <DropdownMenuContent
      data-slot="dropdown-menu-sub-content"
      className={cn("w-auto", className)}
      align={align}
      alignOffset={alignOffset}
      side={side}
      sideOffset={sideOffset}
      {...props}
    />
  )
}

function DropdownMenuCheckboxItem({
  className,
  children,
  checked,
  inset,
  ...props
}: MenuPrimitive.CheckboxItem.Props & {
  inset?: boolean
}) {
  return (
    <MenuPrimitive.CheckboxItem
      data-slot="dropdown-menu-checkbox-item"
      data-inset={inset}
      className={cn(ROW, "pr-8", className)}
      checked={checked}
      {...props}
    >
      <span
        className="ui-dmc__check pointer-events-none absolute right-2.5"
        data-slot="dropdown-menu-checkbox-item-indicator"
      >
        <MenuPrimitive.CheckboxItemIndicator>
          <CheckIcon />
        </MenuPrimitive.CheckboxItemIndicator>
      </span>
      {children}
    </MenuPrimitive.CheckboxItem>
  )
}

function DropdownMenuRadioGroup({ ...props }: MenuPrimitive.RadioGroup.Props) {
  return (
    <MenuPrimitive.RadioGroup
      data-slot="dropdown-menu-radio-group"
      {...props}
    />
  )
}

function DropdownMenuRadioItem({
  className,
  children,
  inset,
  ...props
}: MenuPrimitive.RadioItem.Props & {
  inset?: boolean
}) {
  return (
    <MenuPrimitive.RadioItem
      data-slot="dropdown-menu-radio-item"
      data-inset={inset}
      className={cn(ROW, "pr-8 data-checked:font-medium data-checked:text-(--ui-dmc-ink)", className)}
      {...props}
    >
      <span
        className="ui-dmc__check pointer-events-none absolute right-2.5"
        data-slot="dropdown-menu-radio-item-indicator"
      >
        <MenuPrimitive.RadioItemIndicator>
          <CheckIcon />
        </MenuPrimitive.RadioItemIndicator>
      </span>
      {children}
    </MenuPrimitive.RadioItem>
  )
}

function DropdownMenuSeparator({
  className,
  ...props
}: MenuPrimitive.Separator.Props) {
  return (
    <MenuPrimitive.Separator
      data-slot="dropdown-menu-separator"
      className={cn("ui-dmc__sep", className)}
      {...props}
    />
  )
}

function DropdownMenuShortcut({
  className,
  ...props
}: React.ComponentProps<"span">) {
  return (
    <span
      data-slot="dropdown-menu-shortcut"
      className={cn("ui-dmc__kbd ml-auto", className)}
      {...props}
    />
  )
}

export {
  DropdownMenu,
  DropdownMenuPortal,
  DropdownMenuTrigger,
  DropdownMenuContent,
  DropdownMenuGroup,
  DropdownMenuLabel,
  DropdownMenuItem,
  DropdownMenuCheckboxItem,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuShortcut,
  DropdownMenuSub,
  DropdownMenuSubTrigger,
  DropdownMenuSubContent,
}
