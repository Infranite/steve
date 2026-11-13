/* #/design — the living specimen of visual system v2 "Quiet Precision": every token and base component as it renders
 * right now, in the current theme and language. Colors are read back from the CSS variables, so the page never drifts
 * from index.css; contrast is computed against the content area. */
import { useEffect, useState, type ReactNode } from "react"
import { cn } from "cn"
import { CheckIcon, ChevronDownIcon, FolderIcon, PlusIcon, RotateCcwIcon, SearchIcon, SparklesIcon, WorkflowIcon, XIcon, BellIcon, LayoutGridIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import { Input } from "@/components/ui/input"
import { Switch } from "@/components/ui/switch"
import { Textarea } from "@/components/ui/textarea"
import { BotAvatar, Chips, Composer, Empty, KindTag, Page, Row, Segmented, Status, Tags, ThemeSwitch, Blank, Card, Chevron, EASE, LIFT, Notice, Panel, Pill, Section, Tile } from "@/ui"
import { Select } from "@/view-shared"
import { st, toast, kindTags, isKind } from "@/store"
import { useT } from "@/i18n"

/* ───────── colour read-back ───────── */
function lum(hex: string) {
  const m = hex.replace("#", ""); const c = [0, 2, 4].map((n) => parseInt(m.slice(n, n + 2), 16) / 255).map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4))
  return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}
const ratio = (a: string, b: string) => { const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05) }
function cssHex(v: string) {
  const el = document.createElement("span"); el.style.color = `var(${v})`; document.body.appendChild(el)
  const c = getComputedStyle(el).color; el.remove()
  const m = c.match(/[\d.]+/g)?.map(Number) ?? [0, 0, 0]
  return "#" + m.slice(0, 3).map((n) => Math.round(n).toString(16).padStart(2, "0")).join("")
}

type T = ReturnType<typeof useT>
const TYPE = (t: T): [string, string, string, string][] => [
  ["type-display", "28/36 · 600", t("views:design.typeDisplay"), t("views:design.sampleDisplay")],
  ["type-title", "20/28 · 600", t("views:design.typeTitle"), t("views:design.sampleTitle")],
  ["type-headline", "15/22 · 600", t("views:design.typeHeadline"), t("views:design.sampleHeadline")],
  ["type-chat", "15/26 · 400", t("views:design.typeChat"), t("views:design.sampleChat")],
  ["type-body", "14/22 · 400", t("views:design.typeBody"), t("views:design.sampleBody")],
  ["type-callout", "13/20 · 400", t("views:design.typeCallout"), t("views:design.sampleCallout")],
  ["type-footnote", "12/18 · 400", t("views:design.typeFootnote"), t("views:design.sampleFootnote")],
  ["type-label", "12/16 · 500", t("views:design.typeLabel"), t("views:design.sampleLabel")],
  ["type-code", "12.5/20 · Mono", t("views:design.typeCode"), "let target = state.return_to.unwrap_or(\"/\"); // ST-128"],
]
const ROLES = (t: T): [string, string, string][] => [
  ["--foreground", t("views:design.roleForeground"), t("views:design.roleForegroundUse")], ["--muted-foreground", t("views:design.roleMuted"), t("views:design.roleMutedUse")], ["--tertiary", t("views:design.roleTertiary"), t("views:design.roleTertiaryUse")],
  ["--blue", t("views:design.roleBlue"), t("views:design.roleBlueUse")], ["--success", t("views:design.roleSuccess"), t("views:design.roleSuccessUse")], ["--attention", t("views:design.roleAttention"), t("views:design.roleAttentionUse")], ["--destructive", t("views:design.roleDestructive"), t("views:design.roleDestructiveUse")],
]
const SURF = (t: T): [string, string][] => [["--background", t("views:design.surfBackground")], ["--sidebar", t("views:design.surfSidebar")], ["--card", t("views:design.surfCard")], ["--popover", t("views:design.surfPopover")], ["--muted", t("views:design.surfMuted")], ["--accent", t("views:design.surfAccent")], ["--border", t("views:design.surfBorder")], ["--input", t("views:design.surfInput")]]

/** a top-level part of the specimen: its title, a line about it, and what it shows */
function Part({ title, note, children, id }: { title: string; note?: string; children: ReactNode; id: string }) {
  return (
    <section className="flex flex-col gap-6 scroll-mt-6" data-design-part={id}>
      <div className="border-b pb-3"><h2 className="type-title">{title}</h2>{note && <p className="mt-1 max-w-[60ch] type-callout text-muted-foreground">{note}</p>}</div>
      {children}
    </section>
  )
}
/** a specimen's caption: the token in mono, what it is for */
const Cap = ({ token, children }: { token?: string; children?: ReactNode }) => (
  <div className="flex min-w-0 flex-col gap-0.5">{token && <span className="type-code text-foreground/85">{token}</span>}{children && <span className="type-footnote text-muted-foreground">{children}</span>}</div>
)

/** a cubic-bezier drawn as a small graph */
function Curve({ c, label }: { c: [number, number, number, number]; label: string }) {
  const W = 120, H = 72, p = (x: number, y: number) => `${x * W},${H - y * H}`
  return (
    <div className="flex items-center gap-4">
      <svg viewBox={`-4 -4 ${W + 8} ${H + 8}`} width={W + 8} height={H + 8} className="shrink-0 overflow-visible" aria-hidden>
        <rect x={0} y={0} width={W} height={H} rx={6} className="fill-muted/60" />
        <path d={`M${p(0, 0)} C${p(c[0], c[1])} ${p(c[2], c[3])} ${p(1, 1)}`} className="fill-none stroke-blue" strokeWidth={2} strokeLinecap="round" />
      </svg>
      <Cap token={`cubic-bezier(${c.join(", ")})`}>{label}</Cap>
    </div>
  )
}

/** the motion samples: entering rows, a height that eases, a bar that grows, a card that lifts */
function MotionSamples() {
  const t = useT()
  const [n, setN] = useState(0), [wide, setWide] = useState(false)
  return (
    <div className="grid items-start gap-4 md:grid-cols-2">
      <Card className="flex flex-col gap-3" data-motion="enter">
        <div className="flex items-center justify-between"><span className="type-headline">{t("views:design.motionEnter")}</span><Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => setN((x) => x + 1)} data-motion-replay><RotateCcwIcon />{t("views:design.replay")}</Button></div>
        <div key={n} className="flex flex-col gap-1">
          {[t("views:design.enterRow1"), t("views:design.enterRow2"), t("views:design.enterRow3")].map((x, i) => (
            <div key={x} className="anim-in flex items-center gap-3 rounded-lg bg-muted/60 px-3 py-2 type-callout" style={{ animationDelay: `${i * 50}ms` }}><span className="size-1.5 rounded-full bg-blue" />{x}</div>))}
        </div>
        <Cap token=".anim-in · 180ms ease-(--ease-out)" />
      </Card>
      <Card className="flex flex-col gap-3" data-motion="expand">
        <span className="type-headline">{t("views:design.motionExpand")}</span>
        <details className="group rounded-lg bg-muted/60">
          <summary className="flex cursor-pointer list-none items-center gap-2 px-3 py-2 type-callout [&::-webkit-details-marker]:hidden">
            <ChevronDownIcon className={cn("size-4 text-tertiary transition-transform group-open:rotate-180", EASE)} />{t("views:design.motionExpandTitle")}
          </summary>
          <div className="px-3 pb-3 type-callout text-muted-foreground">{t("views:design.motionExpandBody")}</div>
        </details>
        <Cap token="details · interpolate-size · 260ms" />
      </Card>
      <Card className="flex flex-col gap-3" data-motion="progress">
        <div className="flex items-center justify-between"><span className="type-headline">{t("views:design.motionProgress")}</span><Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => setWide((x) => !x)} data-motion-toggle>{t("views:design.toggle")}</Button></div>
        <div className="flex items-center gap-3">
          <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-muted"><div className="h-full rounded-full bg-blue transition-[width] duration-260 ease-(--ease-out)" style={{ width: wide ? "82%" : "34%" }} /></div>
          <span className="w-10 text-right type-callout tabular-nums text-muted-foreground">{wide ? 82 : 34}%</span>
        </div>
        <Cap token="transition-[width] · 260ms ease-(--ease-out)" />
      </Card>
      <div className={cn(LIFT, "flex cursor-default flex-col gap-3 p-4 transition-[box-shadow,translate] duration-180 ease-(--ease-out) hover:-translate-y-px hover:shadow-[var(--lift-hover)]")} data-motion="hover">
        <span className="type-headline">{t("views:design.motionHover")}</span>
        <span className="type-callout text-muted-foreground">{t("views:design.motionHoverBody")}</span>
        <Cap token="--lift → --lift-hover · 180ms" />
      </div>
    </div>
  )
}

export function DesignView() {
  const t = useT()
  const [, f] = useState(0)
  /* the colours are read from the live CSS: read them again once the theme class has switched */
  useEffect(() => { const tm = setTimeout(() => f((x) => x + 1), 30); return () => clearTimeout(tm) }, [st.ver])
  const [seg, setSeg] = useState("list"), [chip, setChip] = useState(t("views:design.chipAll")), [on, setOn] = useState(true), [check, setCheck] = useState(true), [sel, setSel] = useState("chat")
  const bg = cssHex("--background")
  const kinds = kindTags().slice(0, 4), others = st.tags.filter((x) => !isKind(x)).slice(0, 3).map((x) => x.id)
  const Do = ({ ok: good, children, note }: { ok: boolean; children: ReactNode; note: string }) => (
    <div className="flex flex-col gap-2"><Card className="flex min-h-24 items-center justify-center">{children}</Card>
      <div className="flex items-start gap-1.5 px-1 type-footnote">{good ? <CheckIcon className="mt-0.5 size-3.5 shrink-0 text-success" /> : <XIcon className="mt-0.5 size-3.5 shrink-0 text-destructive" />}<span className="text-muted-foreground">{note}</span></div></div>
  )
  return (
    <Page title={t("views:design.title")} sub={t("views:design.sub")} max={1040} toolbar={<ThemeSwitch labels />}>
      <div className="flex flex-col gap-16" data-design>
        <a href="#/design/components" className={cn(LIFT, "-mb-6 flex items-center gap-4 p-4 transition-[box-shadow,translate] duration-180 ease-(--ease-out) hover:-translate-y-px hover:shadow-[var(--lift-hover)]")} data-design-showroom-link>
          <Tile I={LayoutGridIcon} />
          <div className="min-w-0 flex-1"><div className="type-headline">{t("views:showroom.link")}</div><div className="type-callout text-muted-foreground">{t("views:showroom.linkSub")}</div></div>
          <Chevron />
        </a>
        <Part id="principles" title={t("views:design.principles")}>
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-5">
            {([["p1", "01"], ["p2", "02"], ["p3", "03"], ["p4", "04"], ["p5", "05"]] as const).map(([k, no]) => (
              <Card key={k} className="flex flex-col gap-1.5"><span className="type-code text-blue">{no}</span><div className="type-headline">{t(`views:design.${k}`)}</div><div className="type-callout text-muted-foreground">{t(`views:design.${k}Desc`)}</div></Card>
            ))}
          </div>
        </Part>

        <Part id="type" title={t("views:design.type")} note={t("views:design.typeNote")}>
          <Panel>{TYPE(t).map(([c, spec, use, sample]) => (
            <div key={c} className="flex flex-col gap-2 px-3 py-4 sm:flex-row sm:items-baseline sm:gap-6" data-type-sample={c}>
              <div className="w-48 shrink-0"><Cap token={c}>{spec} · {use}</Cap></div>
              <span className={cn(c, "min-w-0 break-words", c === "type-label" ? "text-tertiary" : c === "type-footnote" || c === "type-callout" ? "text-muted-foreground" : "text-foreground")}>{sample}</span>
            </div>
          ))}</Panel>
          <div className="flex flex-wrap items-baseline gap-x-8 gap-y-2 px-1">
            <span className="type-callout">Geist <span className="font-medium">Medium</span> <span className="font-semibold">Semibold</span></span>
            <span className="font-mono type-callout">Geist Mono 0123456789</span>
            <span className="type-callout tabular-nums text-muted-foreground">tabular-nums 1,284 · 96.0% · $17.23</span>
          </div>
        </Part>

        <Part id="colors" title={t("views:design.colors")} note={t("views:design.colorsNote")}>
          <Section label={t("views:design.neutrals")}>
            <div className="grid grid-cols-2 gap-3 sm:grid-cols-4 lg:grid-cols-8">
              {SURF(t).map(([v, n]) => <div key={v} data-swatch={v}><div className="h-16 rounded-xl shadow-[inset_0_0_0_1px_rgb(0_0_0/0.06)] dark:shadow-[inset_0_0_0_1px_rgb(255_255_255/0.06)]" style={{ background: `var(${v})` }} />
                <div className="mt-2 px-0.5"><div className="type-callout">{n}</div><div className="type-code text-muted-foreground">{cssHex(v).toUpperCase()}</div></div></div>)}
            </div>
          </Section>
          <Section label={t("views:design.roles")}>
            <Panel>{ROLES(t).map(([v, n, use]) => { const hx = cssHex(v), cr = ratio(hx, bg); return (
              <div key={v} className="flex items-center gap-3 px-3 py-2.5">
                <span className="size-8 shrink-0 rounded-full shadow-[inset_0_0_0_1px_rgb(0_0_0/0.08)]" style={{ background: `var(${v})` }} />
                <div className="min-w-0 flex-1"><div className="type-body">{n} <span className="ml-1 type-code text-muted-foreground">{v.slice(2)}</span></div><div className="type-footnote text-muted-foreground">{use}</div></div>
                <span className="hidden type-code text-muted-foreground sm:inline">{hx.toUpperCase()}</span>
                <span className={cn("w-14 text-right type-code", cr >= 4.5 ? "text-foreground" : "text-muted-foreground")} data-contrast={v}>{cr.toFixed(1)}:1</span>
              </div>) })}</Panel>
          </Section>
          <Section label={t("views:design.tints")}>
            <Card className="flex flex-wrap items-center gap-2">
              <Pill tone="info">{t("views:design.toneInfo")}</Pill><Pill tone="attention">{t("views:design.toneAttention")}</Pill><Pill tone="destructive">{t("views:design.toneDestructive")}</Pill><Pill tone="success">{t("views:design.toneSuccess")}</Pill><Pill>{t("views:design.toneNeutral")}</Pill>
              <span className="mx-2 h-4 w-px bg-border" aria-hidden />
              {["run", "wait", "fail", "done"].map((s) => <Status key={s} s={s} pill />)}
            </Card>
          </Section>
        </Part>

        <Part id="shape" title={t("views:design.shape")} note={t("views:design.shapeNote")}>
          <div className="grid grid-cols-2 gap-3 sm:grid-cols-5">
            {([["rounded-md", "6", t("views:design.radius6")], ["rounded-lg", "8", t("views:design.radius8")], ["rounded-xl", "12", t("views:design.radius12")], ["rounded-2xl", "16", t("views:design.radius16")], ["rounded-full", "∞", t("views:design.radiusFull")]] as const).map(([c, px, use]) => (
              <div key={c} className="flex flex-col gap-2"><div className="flex h-20 items-center justify-center rounded-xl bg-muted/60"><div className={cn("size-12 border-2 border-blue/70 bg-blue/10", c)} /></div><Cap token={`${c} · ${px}`}>{use}</Cap></div>
            ))}
          </div>
          <div className="grid gap-4 rounded-2xl bg-muted/60 p-6 sm:grid-cols-4">
            <div className="flex flex-col gap-2"><div className="h-20 rounded-xl bg-background" /><Cap token="--background">{t("views:design.elevPage")}</Cap></div>
            <div className="flex flex-col gap-2"><div className={cn(LIFT, "h-20")} /><Cap token="--lift">{t("views:design.elevCard")}</Cap></div>
            <div className="flex flex-col gap-2"><div className="h-20 rounded-xl bg-popover shadow-md" /><Cap token="shadow-md">{t("views:design.elevPopover")}</Cap></div>
            <div className="flex flex-col gap-2"><div className="h-20 rounded-2xl bg-popover shadow-xl" /><Cap token="shadow-xl">{t("views:design.elevDialog")}</Cap></div>
          </div>
        </Part>

        <Part id="components" title={t("views:design.components")}>
          <Section label={t("views:design.buttons")} note={t("views:design.buttonsNote")}>
            <Card className="flex flex-col gap-5 p-5">
              {(["lg", "default", "sm", "xs"] as const).map((sz) => (
                <div key={sz} className="flex flex-wrap items-center gap-2">
                  <span className="w-16 shrink-0 type-code text-muted-foreground">{sz}</span>
                  <Button size={sz}>{sz === "xs" ? null : <CheckIcon />}{t("views:design.accept")}</Button>
                  <Button size={sz} variant="outline">{t("views:design.rework")}</Button>
                  <Button size={sz} variant="secondary">{t("common:action.edit")}</Button>
                  <Button size={sz} variant="ghost">{t("common:action.cancel")}</Button>
                  <Button size={sz} variant="destructive">{t("views:design.dropItem")}</Button>
                  <Button size={sz} variant="link">{t("views:design.send")}</Button>
                </div>))}
              <div className="flex flex-wrap items-center gap-2">
                <span className="w-16 shrink-0 type-code text-muted-foreground">icon</span>
                <Button size="icon" aria-label={t("common:action.add")}><PlusIcon /></Button><Button size="icon" variant="outline" aria-label={t("views:design.searchItems")}><SearchIcon /></Button><Button size="icon-sm" variant="ghost" aria-label={t("views:design.motionHover")}><SparklesIcon /></Button><Button size="icon-xs" variant="ghost" aria-label={t("common:action.cancel")}><XIcon /></Button>
                <Button disabled>{t("views:design.disabled")}</Button>
              </div>
            </Card>
          </Section>

          <div className="grid gap-8 lg:grid-cols-2">
            <Section label={t("views:design.inputs")}>
              <Card className="flex flex-col gap-3 p-5">
                <div className="relative"><SearchIcon className="pointer-events-none absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-tertiary" /><Input className="pl-8" placeholder={t("views:design.searchItems")} /></div>
                <Input disabled placeholder={t("views:design.disabled")} />
                <Textarea className="min-h-20" placeholder={t("views:design.textareaPlaceholder")} />
                <div className="flex items-center gap-2"><Select testId="design-select" value={sel} onChange={setSel} options={[{ v: "chat", l: t("views:design.viewList") }, { v: "board", l: t("views:design.viewBoard") }, { v: "graph", l: t("views:design.viewGraph") }]} /></div>
              </Card>
            </Section>
            <Section label={t("views:design.choices")}>
              <Card className="flex flex-col gap-4 p-5">
                <Segmented value={seg} onChange={setSeg} options={[["list", t("views:design.viewList")], ["board", t("views:design.viewBoard")], ["graph", t("views:design.viewGraph")]]} />
                <Segmented size="sm" value={seg} onChange={setSeg} options={[["list", t("views:design.viewList")], ["board", t("views:design.viewBoard")], ["graph", t("views:design.viewGraph")]]} />
                <Chips value={chip} onChange={setChip} options={[t("views:design.chipAll"), t("views:design.chipMine"), t("views:design.chipWaiting")]} />
                <label className="flex items-center justify-between gap-3 type-body">{t("views:design.switchLabel")}<Switch checked={on} onCheckedChange={setOn} /></label>
                <label className="flex items-center gap-2.5 type-body"><Checkbox checked={check} onCheckedChange={(v) => setCheck(!!v)} />{t("views:design.checkLabel")}</label>
              </Card>
            </Section>
          </div>

          <Section label={t("views:design.composerLabel")}>
            <Composer placeholder={t("views:design.composerPlaceholder")} onSend={() => toast(t("views:design.composerToast"))} />
          </Section>

          <Section label={t("views:design.tags")} note={t("views:design.tagsNote")}>
            <Card className="flex flex-col gap-4 p-5">
              <div className="flex flex-wrap items-center gap-2">{kinds.map((k) => <KindTag key={k.id} id={k.id} />)}{others.length > 0 && <Tags ids={others} />}</div>
              <div className="flex flex-wrap items-center gap-x-6 gap-y-2">{["run", "wait", "prop", "pause", "done", "drop"].map((s) => <Status key={s} s={s} className="type-callout" />)}</div>
              <div className="flex flex-wrap items-center gap-x-6 gap-y-2">{["run", "wait", "idle", "ended"].map((s) => <Status key={s} s={s} sess className="type-callout" />)}</div>
            </Card>
          </Section>

          <Section label={t("views:design.rows")} note={t("views:design.rowsNote")}>
            <Panel>
              <Row icon={<BotAvatar k="steve" size={36} />} title={<span className="type-headline">{st.bots.steve?.n ?? "Steve"}</span>} sub={t("views:design.sampleFootnote")} onClick={() => {}}><Chevron /></Row>
              <Row icon={<Tile I={FolderIcon} />} title={t("views:design.rowOpen")} sub={t("views:design.rowOpenSub")}><Select align="end" testId="design-row-select" value={sel} onChange={setSel} options={[{ v: "chat", l: t("views:design.viewList") }, { v: "board", l: t("views:design.viewBoard") }, { v: "graph", l: t("views:design.viewGraph") }]} /></Row>
              <Row icon={<Tile I={BellIcon} />} title={t("views:design.rowSwitch")} sub={t("views:design.rowSwitchSub")}><Switch checked={on} onCheckedChange={setOn} /></Row>
              <Row icon={<Tile I={WorkflowIcon} />} title={t("views:design.rowHarness")}><span className="type-callout text-muted-foreground">Claude Code · <span className="type-code">gpt-6.1-sol</span></span></Row>
            </Panel>
          </Section>

          <Section label={t("views:design.cards")}>
            <div className="grid items-start gap-4 lg:grid-cols-2">
              <Card className="flex flex-col gap-3 p-5">
                <div className="flex items-center gap-2"><Status s="wait" pill /><span className="type-code text-muted-foreground">ST-128</span></div>
                <div><div className="type-headline">{t("views:design.cardTitle")}</div><p className="mt-1 type-body text-muted-foreground">{t("views:design.cardBody")}</p></div>
                <div className="flex gap-2"><Button size="sm">{t("views:design.accept")}</Button><Button size="sm" variant="outline">{t("views:design.rework")}</Button></div>
              </Card>
              <div className="flex flex-col gap-2">
                <Notice tone="info">{t("views:design.noticeInfo")}</Notice>
                <Notice tone="attention">{t("views:design.noticeAttention")}</Notice>
                <Notice tone="destructive">{t("views:design.noticeDestructive")}</Notice>
                <Notice tone="success">{t("views:design.noticeSuccess")}</Notice>
                <Blank>{t("views:design.blank")}</Blank>
              </div>
            </div>
          </Section>

          <Section label={t("views:design.empty")}>
            <Card className="p-0"><Empty icon={<WorkflowIcon />} title={t("views:design.emptyTitle")} sub={t("views:design.emptySub")} action={<Button variant="outline"><PlusIcon />{t("views:design.emptyAction")}</Button>} /></Card>
          </Section>
        </Part>

        <Part id="motion" title={t("views:design.motion")} note={t("views:design.motionNote")}>
          <Card className="grid gap-4 p-5 sm:grid-cols-2">
            <Curve c={[0.16, 1, 0.3, 1]} label={t("views:design.easeOut")} />
            <Curve c={[0.65, 0, 0.35, 1]} label={t("views:design.easeInOut")} />
          </Card>
          <MotionSamples />
        </Part>

        <Part id="dos" title={t("views:design.dos")}>
          <div className="grid gap-x-4 gap-y-6 sm:grid-cols-2">
            <Do ok note={t("views:design.doStatus")}><div className="flex flex-col items-center gap-3"><div className="flex gap-4"><Status s="run" className="type-callout" /><Status s="wait" className="type-callout" /><Status s="done" className="type-callout" /></div><div className="flex gap-2"><Status s="wait" pill /><Pill tone="attention">{t("views:design.toneAttention")}</Pill></div></div></Do>
            {/* the counter-examples keep their loud colours on purpose: they show what not to do */}
            <Do ok={false} note={t("views:design.dontStatus")}><div className="flex flex-wrap gap-2">{[[t("views:design.tagRun"), "#0071e3"], [t("views:design.tagWait"), "#ff9500"], [t("views:design.tagDone"), "#28a745"], [t("views:design.tagProp"), "#9a5cff"]].map(([l, c]) => <span key={l} className="rounded-md px-2 py-0.5 type-label text-white" style={{ background: c }}>{l}</span>)}</div></Do>
            <Do ok note={t("views:design.doAvatar")}><div className="flex gap-2">{Object.keys(st.bots).slice(0, 5).map((k) => <BotAvatar key={k} k={k} size={32} />)}</div></Do>
            <Do ok={false} note={t("views:design.dontAvatar")}><div className="flex gap-2">{["#5b8def", "#e0703a", "#2fa36b", "#9a5cff", "#d6a23a"].map((c, n) => <span key={c} className="flex size-8 items-center justify-center rounded-lg type-callout font-semibold text-white" style={{ background: c }}>{[...t("views:design.botInitials")][n]}</span>)}</div></Do>
            <Do ok note={t("views:design.doButtons")}><div className="flex gap-2"><Button variant="ghost">{t("views:design.noReply")}</Button><Button variant="outline">{t("views:design.moreFormal")}</Button><Button>{t("views:design.send")}</Button></div></Do>
            <Do ok={false} note={t("views:design.dontButtons")}><div className="flex gap-2">{[[t("views:design.noReply"), "#e5352b"], [t("views:design.rewrite"), "#ff9500"], [t("views:design.send"), "#28a745"]].map(([l, c]) => <span key={l} className="rounded-lg px-3 py-1.5 type-callout font-medium text-white" style={{ background: c }}>{l}</span>)}</div></Do>
            <Do ok note={t("views:design.doHint")}><span className="type-callout text-muted-foreground">{t("views:item.goalHint")}</span></Do>
            <Do ok={false} note={t("views:design.dontHint")}><span className="rounded-lg bg-[#1a56db] px-3 py-2 type-callout font-medium text-white">{t("views:design.hintBox")}</span></Do>
          </div>
        </Part>
      </div>
    </Page>
  )
}
