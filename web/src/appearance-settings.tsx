/* 设置 · 外观 · 配色与字体: the accent, the neutrals, the fonts and the corner radius, chosen on this device (web/src/themes).
 * Every choice shows at once; the preview card above the choices is drawn with the live tokens, so it is the page
 * itself in miniature. Themes registered by others (registerTheme) appear among the built-ins of their kind. */
import { useRef, type CSSProperties, type KeyboardEvent, type ReactNode } from "react"
import { cn } from "cn"
import { CheckIcon, CircleDotIcon, InboxIcon, MessageSquareIcon, RotateCcwIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { lang, useT } from "@/i18n"
import { useStore } from "@/store"
import { Card, Section, Segmented, Pill, resolvedTheme } from "@/ui"
import { chosen, DEFAULT_APPEARANCE, resetAppearance, setAppearance, themeName, themesFor, useAppearance, type Manifest, type Slot } from "@/themes"

type T = ReturnType<typeof useT>
/** a theme's name in the language of the moment (`t` ties the call to a component that re-renders on a language change) */
export const nameOf = (_t: T, m: Manifest) => themeName(m, lang())

/** a row of choices as a radio group: arrows move the choice, as in any radio group */
function Choices({ slot, label, value, items, render, className }: { slot: Slot; label: string; value: string; items: Manifest[]; render: (m: Manifest, on: boolean) => ReactNode; className?: string }) {
  const t = useT()
  const ref = useRef<HTMLDivElement>(null)
  const cur = Math.max(0, items.findIndex((m) => m.id === value))
  const key = (e: KeyboardEvent) => {
    const d = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : e.key === "ArrowLeft" || e.key === "ArrowUp" ? -1 : 0
    if (!d) return
    e.preventDefault()
    const n = (cur + d + items.length) % items.length
    setAppearance({ [slot]: items[n].id })
    ref.current?.querySelectorAll<HTMLButtonElement>("[role=radio]")[n]?.focus()
  }
  return (
    <div ref={ref} role="radiogroup" aria-label={label} onKeyDown={key} className={cn("flex flex-wrap gap-x-1 gap-y-2", className)} data-appearance={slot}>
      {items.map((m, i) => {
        const on = i === cur
        return (
          <button key={m.id} type="button" role="radio" aria-checked={on} tabIndex={on ? 0 : -1} title={nameOf(t, m)} data-choice={m.id} onClick={() => setAppearance({ [slot]: m.id })}
            className="group/choice flex w-16 flex-col items-center gap-1.5 rounded-lg px-1 pt-1.5 pb-1 outline-none hover:bg-accent focus-visible:ring-[3px] focus-visible:ring-ring/50">
            {render(m, on)}
            <span className={cn("max-w-full truncate type-footnote", on ? "font-medium text-foreground" : "text-muted-foreground")}>{nameOf(t, m)}</span>
          </button>)
      })}
    </div>
  )
}
const mode = () => resolvedTheme() as "light" | "dark"
/** an accent: its primary as a filled circle, a check in its own text colour when chosen */
function AccentDot({ m, on }: { m: Manifest; on: boolean }) {
  const tk = m.tokens?.[mode()] ?? {}
  return (
    <span className={cn("flex size-8 items-center justify-center rounded-full shadow-[inset_0_0_0_1px_rgb(0_0_0/0.08)] transition-shadow duration-120", on && "shadow-[0_0_0_2px_var(--card),0_0_0_4px_var(--swatch)]")}
      style={{ background: tk.primary, "--swatch": tk.primary } as CSSProperties} aria-hidden>
      {on && <CheckIcon className="size-4" strokeWidth={2.5} style={{ color: tk["primary-foreground"] }} />}
    </span>
  )
}
/** a neutral: a page in its colours, with a card, a line of ink and a line of secondary text on it */
function NeutralTile({ m, on }: { m: Manifest; on: boolean }) {
  const tk = m.tokens?.[mode()] ?? {}
  return (
    <span className={cn("relative block h-10 w-14 overflow-hidden rounded-lg shadow-[inset_0_0_0_1px_rgb(0_0_0/0.1)] dark:shadow-[inset_0_0_0_1px_rgb(255_255_255/0.1)]", on && "ring-2 ring-primary ring-offset-2 ring-offset-card")} style={{ background: tk.background }} aria-hidden>
      <span className="absolute inset-y-0 left-0 w-3" style={{ background: tk.sidebar, boxShadow: `1px 0 0 ${tk["sidebar-border"]}` }} />
      <span className="absolute top-2 right-1.5 bottom-0 left-4.5 rounded-tl-md rounded-tr-md" style={{ background: tk.card, boxShadow: `0 0 0 1px ${tk.border}` }}>
        <span className="absolute top-1.5 left-1.5 h-1 w-5 rounded-full" style={{ background: tk.foreground }} />
        <span className="absolute top-3.5 left-1.5 h-1 w-7 rounded-full" style={{ background: tk["muted-foreground"], opacity: 0.6 }} />
        <span className="absolute top-5.5 left-1.5 h-1 w-4 rounded-full" style={{ background: tk.muted }} />
      </span>
    </span>
  )
}

/** the page in miniature, in the live tokens: sidebar, a title, text, a link, a status, a code chip, a field, buttons */
function Preview() {
  const t = useT()
  return (
    <div className="overflow-hidden rounded-xl bg-background shadow-[inset_0_0_0_1px_var(--border)]" data-appearance-preview aria-label={t("manage:look.preview")} role="img">
      <div className="flex">
        <div className="hidden w-36 shrink-0 flex-col gap-0.5 border-r border-sidebar-border bg-sidebar p-2 sm:flex" aria-hidden>
          <span className="flex items-center gap-2 rounded-md bg-sidebar-accent px-2 py-1 type-callout font-medium text-sidebar-foreground"><MessageSquareIcon className="size-3.5" />{t("manage:look.pvChat")}</span>
          <span className="flex items-center gap-2 rounded-md px-2 py-1 type-callout text-muted-foreground"><InboxIcon className="size-3.5" />{t("manage:look.pvInbox")}</span>
          <span className="flex items-center gap-2 rounded-md px-2 py-1 type-callout text-muted-foreground"><CircleDotIcon className="size-3.5" />{t("manage:look.pvItems")}</span>
        </div>
        <div className="flex min-w-0 flex-1 flex-col gap-3 p-4" aria-hidden>
          <div className="flex flex-wrap items-center gap-2">
            <span className="type-code text-muted-foreground">ST-27</span>
            <span className="min-w-0 type-headline">{t("manage:look.pvTitle")}</span>
            <Pill tone="info"><span className="size-1.5 rounded-full bg-blue dot-running" />{t("manage:look.pvRunning")}</Pill>
          </div>
          <div className="rounded-xl bg-card p-3 shadow-[var(--lift)]">
            <p className="type-callout text-foreground">{t("manage:look.pvBody")} <span className="text-blue underline-offset-2 hover:underline">{t("manage:look.pvLink")}</span></p>
            <p className="mt-1 type-footnote text-muted-foreground">{t("manage:look.pvMeta")} · <code className="rounded-sm bg-muted px-1 font-mono">git rebase dev</code></p>
          </div>
          <div className="flex flex-wrap items-center gap-2">
            <span className="flex h-8 min-w-0 flex-1 basis-40 items-center rounded-lg bg-muted px-2.5 type-callout text-tertiary shadow-[inset_0_0_0_1px_var(--input)]">{t("manage:look.pvInput")}</span>
            <Button size="sm" variant="outline" tabIndex={-1}>{t("manage:look.pvLater")}</Button>
            <Button size="sm" tabIndex={-1}>{t("manage:look.pvAction")}</Button>
          </div>
        </div>
      </div>
    </div>
  )
}

/** a labelled block of choices inside the card */
const Block = ({ label, sub, children }: { label: string; sub?: string; children: ReactNode }) => (
  <div className="flex flex-col gap-2.5 px-4 py-4">
    <div><div className="type-body">{label}</div>{sub && <div className="type-footnote text-muted-foreground">{sub}</div>}</div>
    {children}
  </div>
)

export function AppearanceSection() {
  const t = useT()
  useStore() // the light/dark choice: the swatches show the mode on screen
  const a = useAppearance()
  const changed = (Object.keys(DEFAULT_APPEARANCE) as Slot[]).some((k) => a[k] !== DEFAULT_APPEARANCE[k])
  const opt = (m: Manifest, style: "sans" | "mono" | null) => [m.id, <span key={m.id} style={style ? { fontFamily: m.fonts?.[style] } : undefined}>{nameOf(t, m)}</span>, nameOf(t, m)] as const
  const skins = themesFor("skin")
  return (
    <Section label={t("manage:look.palette")} note={t("manage:look.paletteNote")} data-appearance-section
      right={changed ? <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={resetAppearance} data-appearance-reset><RotateCcwIcon />{t("manage:look.reset")}</Button> : undefined}>
      <Card className="p-0">
        <div className="p-3"><Preview /></div>
        <div className="divide-y divide-border">
          <Block label={t("manage:look.accent")} sub={t("manage:look.accentSub")}>
            <Choices slot="accent" label={t("manage:look.accent")} value={a.accent} items={themesFor("accent")} render={(m, on) => <AccentDot m={m} on={on} />} className="-mx-1" />
          </Block>
          <Block label={t("manage:look.neutral")} sub={t("manage:look.neutralSub")}>
            <Choices slot="neutral" label={t("manage:look.neutral")} value={a.neutral} items={themesFor("neutral")} render={(m, on) => <NeutralTile m={m} on={on} />} className="-mx-1" />
          </Block>
          <Block label={t("manage:look.font")}>
            <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
              <span className="type-callout text-muted-foreground">{t("manage:look.fontSans")}</span>
              <span data-appearance="sans" className="contents"><Segmented size="sm" label={t("manage:look.fontSans")} value={a.sans} onChange={(v) => setAppearance({ sans: v })} options={themesFor("sans").map((m) => opt(m, "sans"))} /></span>
            </div>
            <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
              <span className="type-callout text-muted-foreground">{t("manage:look.fontMono")}</span>
              <span data-appearance="mono" className="contents"><Segmented size="sm" label={t("manage:look.fontMono")} value={a.mono} onChange={(v) => setAppearance({ mono: v })} options={themesFor("mono").map((m) => opt(m, "mono"))} /></span>
            </div>
          </Block>
          <Block label={t("manage:look.radius")}>
            <span data-appearance="radius" className="contents"><Segmented size="sm" label={t("manage:look.radius")} value={a.radius} onChange={(v) => setAppearance({ radius: v })} className="self-start"
              options={themesFor("radius").sort((x, y) => (x.radius ?? 1) - (y.radius ?? 1)).map((m) => [m.id, <span key={m.id} className="inline-flex items-center gap-1.5"><span aria-hidden className="size-3 border-t-[1.5px] border-l-[1.5px] border-current" style={{ borderTopLeftRadius: Math.round(6 * (m.radius ?? 1)) }} />{nameOf(t, m)}</span>, nameOf(t, m)] as const)} /></span>
          </Block>
          {skins.length > 0 && <Block label={t("manage:look.skin")} sub={t("manage:look.skinSub")}>
            <Segmented size="sm" label={t("manage:look.skin")} value={skins.some((m) => m.id === a.skin) ? a.skin : ""} onChange={(v) => setAppearance({ skin: v })} className="self-start"
              options={[["", t("manage:look.skinNone"), t("manage:look.skinNone")] as const, ...skins.map((m) => opt(m, null))]} />
          </Block>}
        </div>
      </Card>
    </Section>
  )
}

/** the current choice in words, for the 视觉规范 page: accent · neutral · font · mono · corners (and the skin) */
export function appearanceLabel(t: T) {
  return (["accent", "neutral", "sans", "mono", "radius", "skin"] as Slot[]).flatMap((s) => { const m = chosen(s); return m ? [nameOf(t, m)] : [] }).join(" · ")
}
