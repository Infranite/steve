import { Children, isValidElement, type ComponentPropsWithRef, type ReactElement, type ReactNode } from "react"
import { Button as ButtonPrimitive } from "@base-ui/react/button"
import { cn } from "cn"

import { Button as AffectButton, buttonCleanCss } from "@/components/affect/button-clean"
import { IconButton } from "@/components/affect/icon-button-clean"

/* Steve's Button, drawn by affectUI's Button (and its Icon Button for the square icon-only sizes). The exports and
 * props stay Steve's, so every call site keeps working: variant and size map onto affectUI's, className still wins over
 * the component's own styles (they sit in Tailwind's components layer), icons given as children go into affectUI's
 * icon slots, and a Button that renders as another element (render, nativeButton={false}) keeps base-ui's primitive
 * with the same affectUI look. Steve's own extras – the attention and link tones, the 36px lg size, the muted
 * secondary – are in styles/affectui-forms.css. */

type Variant = "default" | "outline" | "secondary" | "ghost" | "destructive" | "attention" | "link"
type Size = "default" | "xs" | "sm" | "lg" | "icon" | "icon-xs" | "icon-sm" | "icon-lg"

const TONE: Record<Variant, "primary" | "secondary" | "ghost" | "destructive"> = {
  default: "primary", outline: "secondary", secondary: "secondary", ghost: "ghost", destructive: "destructive", attention: "primary", link: "ghost",
}
/* Steve's heights 24 · 28 · 32 · 36 onto affectUI's 24 · 28 · 32 (36 is Steve's own: data-steve-size="lg") */
const SCALE: Record<Size, "sm" | "md" | "lg"> = {
  xs: "sm", sm: "md", default: "lg", lg: "lg", "icon-xs": "sm", "icon-sm": "md", icon: "lg", "icon-lg": "lg",
}

type ButtonProps = ButtonPrimitive.Props & { variant?: Variant | null; size?: Size | null }

/* An icon child: a component element with no children and nothing but an icon's props (lucide's, Spinner's). An
 * avatar, a Kbd, or an icon that hides itself at some width stays in the label as it is. */
const ICON_PROPS = new Set(["className", "size", "strokeWidth", "absoluteStrokeWidth", "aria-hidden", "data-icon", "style", "color"])
type IconElement = ReactElement<{ className?: string; "data-icon"?: string }>
function isIcon(n: ReactNode): n is IconElement {
  if (!isValidElement(n) || typeof n.type === "string" || typeof n.type === "symbol") return false
  const props = n.props as Record<string, unknown>
  if (props.children != null) return false
  if (!Object.keys(props).every((k) => ICON_PROPS.has(k))) return false
  return !/(^|\s|:)hidden(\s|$)/.test(String(props.className ?? ""))
}

/* Leading and trailing icons out of the children; what is left is the label */
function split(children: ReactNode) {
  const parts = Children.toArray(children)
  let icon: ReactNode, iconRight: ReactNode
  const first = parts[0]
  if (isIcon(first) && first.props["data-icon"] !== "inline-end") icon = parts.shift()
  const last = parts[parts.length - 1]
  if (isIcon(last) && (parts.length > 1 || last.props["data-icon"] === "inline-end")) iconRight = parts.pop()
  const label = parts.length === 0 ? undefined : parts.length === 1 ? parts[0] : parts
  return { icon, iconRight, label }
}

function Button({ className, variant, size, render, nativeButton, focusableWhenDisabled, children, ...props }: ButtonProps) {
  const v: Variant = variant ?? "default"
  const s: Size = size ?? "default"
  const steve = { "data-slot": "button", "data-steve-variant": v, "data-steve-size": s }
  const square = s.startsWith("icon")

  // Another element (a link), or base-ui's disabled-but-focusable: the primitive, in affectUI's look
  if (render || nativeButton === false || focusableWhenDisabled) {
    return (
      <>
        <style href="ui-button-clean" precedence="default">{buttonCleanCss}</style>
        <ButtonPrimitive
          {...steve}
          render={render}
          nativeButton={nativeButton}
          focusableWhenDisabled={focusableWhenDisabled}
          className={typeof className === "function" ? (st) => cn("ui-buttonc", className(st)) : cn("ui-buttonc", className)}
          data-variant={TONE[v]}
          data-size={SCALE[s]}
          data-icon-only={square || undefined}
          {...props}
        >
          {children}
        </ButtonPrimitive>
      </>
    )
  }

  const rest = props as Omit<ComponentPropsWithRef<"button">, "children" | "className">
  const cls = typeof className === "string" ? className : undefined
  const { icon, iconRight, label } = split(children as ReactNode)

  // A square button that is only an icon and names what it does: affectUI's Icon Button. The name is its accessible
  // label; the tooltip stays the caller's (a title, or a Tooltip around it), so nothing shows two.
  const name = rest["aria-label"]
  if (square && icon && !iconRight && label === undefined && typeof name === "string" && name && TONE[v] !== "destructive") {
    return (
      <IconButton
        {...steve}
        {...rest}
        icon={icon}
        label={name}
        title={rest.title ?? ""}
        variant={TONE[v] as "primary" | "secondary" | "ghost"}
        size={SCALE[s] === "sm" ? "sm" : "md"}
        className={cls}
      />
    )
  }

  return (
    <AffectButton
      {...steve}
      {...rest}
      variant={TONE[v]}
      size={SCALE[s]}
      icon={icon}
      iconRight={iconRight}
      data-square={square && label !== undefined ? "" : undefined}
      className={cls}
    >
      {label}
    </AffectButton>
  )
}

export { Button }
export type { ButtonProps }
