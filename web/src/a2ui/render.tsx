/* A2UI v0.9.1 renderer (docs/ST-46-a2ui.md): the basic catalog plus Steve's `Chart`, drawn with the app's own controls and
 * the stats page's chart. A ```a2ui block in a reply becomes its surfaces; an event button sends one controlled action
 * through the Hub (`a2uiSubmit`), which keeps one submission per surface. Where no reply hosts the block (an item's feed,
 * an IM conversation, a draft) it is drawn read-only. Nothing an agent writes runs as code or HTML. */
import { createContext, useContext, useEffect, useMemo, useRef, useState, type ComponentType, type ReactNode } from "react"
import { cn } from "cn"
import * as L from "lucide-react"
import type { A2uiRecord } from "@/api"
import { Markdown } from "@/components/markdown"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group"
import { Dialog, DialogContent, DialogTitle } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { Textarea } from "@/components/ui/textarea"
import { Chart, ChartHead, HBars, seriesColor } from "@/stats/chart"
import { shortDay } from "@/stats/format"
import { a2uiSubmit, toast } from "@/store"
import {
  absolute, actionKey, blockKey, chartRows, childrenOf, eventContext, failedChecks, getPointer, isBinding, LIMITS, parseBlock, resolve, setPointer, str, STEVE,
  type Comp, type Problem, type Scope, type Surface,
} from "@/a2ui/model"
import { A2uiPending, HostCtx, Nested } from "@/a2ui/host"
import { useT } from "@/i18n"
import { date, num } from "@/i18n/format"

/* ───────── the block ───────── */
/** a ```a2ui code block: a placeholder while it is written, the surfaces once it is closed, a note when it can't be drawn */
export function A2uiBlock({ src, streaming, code }: { src: string; streaming?: boolean; code: ReactNode }) {
  const doc = useMemo(() => (streaming ? null : parseBlock(src)), [src, streaming])
  const block = useMemo(() => blockKey(src), [src])
  if (!doc) return <A2uiPending />
  if (doc.fatal) return <Broken why={doc.fatal} problems={doc.problems} code={code} />
  return (
    <div className="my-3 flex flex-col gap-3" data-a2ui={block}>
      {doc.surfaces.map((s) => <SurfaceView key={s.id} s={s} block={block} />)}
      {doc.problems.length > 0 && <Problems list={doc.problems} />}
    </div>
  )
}

function Broken({ why, problems, code }: { why: string; problems: Problem[]; code: ReactNode }) {
  const t = useT(), [open, setOpen] = useState(false)
  return (
    <div className="my-3 rounded-xl bg-attention/10 px-4 py-3" data-a2ui-broken>
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1 type-callout">
        <L.TriangleAlertIcon className="size-4 shrink-0 text-attention" aria-hidden />
        <span className="text-foreground">{t("misc:a2ui.broken", { why })}</span>
        <button type="button" className="type-footnote text-muted-foreground hover:text-foreground" aria-expanded={open} onClick={() => setOpen(!open)}>{open ? t("misc:a2ui.hide") : t("misc:a2ui.showSource")}</button>
      </div>
      {open && <div className="mt-2">{problems.length > 0 && <ProblemList list={problems} />}<div className="[&_[data-streamdown=code-block]]:mb-0">{code}</div></div>}
    </div>
  )
}
function Problems({ list }: { list: Problem[] }) {
  const t = useT(), [open, setOpen] = useState(false)
  return (
    <div className="type-footnote text-muted-foreground" data-a2ui-problems>
      <button type="button" className="hover:text-foreground" aria-expanded={open} onClick={() => setOpen(!open)}>{open ? t("misc:a2ui.problemsOpen", { count: list.length }) : t("misc:a2ui.problems", { count: list.length })}</button>
      {open && <ProblemList list={list} />}
    </div>
  )
}
const ProblemList = ({ list }: { list: Problem[] }) => (
  <ul className="mt-1 flex flex-col gap-0.5 type-footnote text-muted-foreground">{list.slice(0, 20).map((p, i) => <li key={i}><code className="font-mono">{p.path}</code> {p.message}</li>)}</ul>
)

/* ───────── a surface: its data model and its one submission ───────── */
type Run = {
  s: Surface; data: unknown; set: (ptr: string, v: unknown) => void
  /** what this surface submitted (it is then read-only), and whether it can submit at all */
  done?: A2uiRecord; locked: string | null; busy: string | null
  fire: (c: Comp, scope: Scope, label: string) => Promise<void>
  /** a Modal's trigger opens it instead of firing its own action */
  trigger?: () => void
}
const RunCtx = createContext<Run | null>(null)
const draftKey = (msg: string, block: string, surface: string) => `steve.a2ui.${msg}.${block}.${surface}`

function SurfaceView({ s, block }: { s: Surface; block: string }) {
  const t = useT(), host = useContext(HostCtx), key = actionKey(block, s.id)
  const done = host?.records[key]
  /* what the inputs show: the values submitted, else what you typed before a reload, else the model as written */
  const [data, setData] = useState<unknown>(() => {
    if (done?.data !== undefined) return done.data
    if (host) try { const d = localStorage.getItem(draftKey(host.msg, block, s.id)); if (d) return JSON.parse(d) } catch { /* private mode */ }
    return s.data
  })
  useEffect(() => { if (done?.data !== undefined) setData(done.data) }, [done])
  const [busy, setBusy] = useState<string | null>(null)
  const inflight = useRef(false)
  const locked = done ? t("misc:a2ui.submitted") : host?.readonly ?? (host ? null : t("misc:a2ui.mainOnly"))
  const set = (ptr: string, v: unknown) => setData((d: unknown) => {
    const next = setPointer(d, ptr, v)
    if (host) try { localStorage.setItem(draftKey(host.msg, block, s.id), JSON.stringify(next)) } catch { /* full or private */ }
    return next
  })
  const fire = async (c: Comp, scope: Scope, label: string) => {
    const a = (c.action ?? {}) as { event?: { name?: unknown; context?: unknown }; functionCall?: { call?: unknown; args?: Record<string, unknown> } }
    if (a.functionCall?.call === "openUrl") {
      const url = str(resolve(a.functionCall.args?.url, scope))
      if (/^https?:\/\//i.test(url)) window.open(url, "_blank", "noopener,noreferrer"); else toast(t("misc:a2ui.httpOnly"))
      return
    }
    if (!a.event || typeof a.event.name !== "string") return
    if (!host || locked || inflight.current) return
    inflight.current = true; setBusy(c.id)
    try {
      const r = await a2uiSubmit(host.msg, {
        block, surfaceId: s.id, sourceComponentId: c.id, name: a.event.name, label, context: eventContext(a.event.context, scope), data, timestamp: new Date().toISOString(),
      })
      if (r.already) toast(t("misc:a2ui.already"))
      try { localStorage.removeItem(draftKey(host.msg, block, s.id)) } catch { /* ignore */ }
    } catch (e) {
      toast(t("misc:a2ui.submitFailed", { error: (e as Error).message || t("misc:a2ui.network") }))
    } finally { inflight.current = false; setBusy(null) }
  }
  const run: Run = { s, data, set, done, locked, busy, fire }
  return (
    <RunCtx.Provider value={run}>
      <Nested.Provider value>
        <section className="flex min-w-0 flex-col gap-2" data-a2ui-surface={s.id} data-a2ui-state={done ? "submitted" : locked ? "readonly" : "open"}>
          <Node id="root" base="/" depth={0} trail="" />
          {done && <div className="flex items-center gap-1.5 type-footnote text-muted-foreground" data-a2ui-done><L.CircleCheckIcon className="size-3.5 text-success" aria-hidden />{done.at ? t("misc:a2ui.doneAt", { label: done.label || done.name, time: date(done.at, { month: "numeric", day: "numeric", hour: "2-digit", minute: "2-digit" }) }) : t("misc:a2ui.done", { label: done.label || done.name })}</div>}
          {!done && locked && <div className="type-footnote text-muted-foreground" data-a2ui-readonly>{locked}</div>}
        </section>
      </Nested.Provider>
    </RunCtx.Provider>
  )
}

/* ───────── components ───────── */
type P = { c: Comp; scope: Scope; depth: number; trail: string }
const useRun = () => useContext(RunCtx)!

function Node({ id, base, depth, trail }: { id: string; base: string; depth: number; trail: string }) {
  const t = useT(), run = useRun()
  const c = run.s.comps.get(id)
  const here = `${id}@${base}`
  /* a missing child is not written yet (or never will be): skip it; a loop or an absurd depth stops */
  if (!c || trail.split("|").includes(here)) return null
  if (depth > LIMITS.depth) return <Unsupported what={t("misc:a2ui.tooDeep", { n: LIMITS.depth })} />
  const scope: Scope = { data: run.data, base }
  const R = (run.s.catalog === STEVE ? STEVE_COMPONENTS : BASIC_COMPONENTS)[c.component]
  if (!R) return <Unsupported what={t("misc:a2ui.unsupported", { name: c.component })} />
  return <R c={c} scope={scope} depth={depth} trail={`${trail}|${here}`} />
}
const Child = ({ id, scope, depth, trail }: { id: unknown; scope: Scope; depth: number; trail: string }) =>
  typeof id === "string" ? <Node id={id} base={scope.base} depth={depth + 1} trail={trail} /> : null
function Kids({ c, scope, depth, trail }: P) {
  return <>{childrenOf(c.children, scope).map((k) => <Weighted key={k.key} id={k.id}><Node id={k.id} base={k.base} depth={depth + 1} trail={trail} /></Weighted>)}</>
}
/* `weight` is flex-grow inside a Row or Column */
function Weighted({ id, children }: { id: string; children: ReactNode }) {
  const w = useRun().s.comps.get(id)?.weight
  return typeof w === "number" && w > 0 ? <div className="min-w-0" style={{ flex: `${w} 1 0` }}>{children}</div> : <>{children}</>
}
const Unsupported = ({ what }: { what: string }) => <div className="rounded-md bg-muted/60 px-2.5 py-1.5 type-footnote text-muted-foreground" data-a2ui-unsupported>{what}</div>
const a11y = (c: Comp, s: Scope) => { const a = c.accessibility as { label?: unknown } | undefined; return a?.label !== undefined ? str(resolve(a.label, s)) : undefined }

const JUSTIFY: Record<string, string> = { start: "justify-start", center: "justify-center", end: "justify-end", spaceBetween: "justify-between", spaceAround: "justify-around", spaceEvenly: "justify-evenly", stretch: "justify-stretch" }
const ALIGN: Record<string, string> = { start: "items-start", center: "items-center", end: "items-end", stretch: "items-stretch" }

const MD = /[*_`#>|[\]\n]|^\s*[-+]\s|^\s*\d+\.\s/m
function Text({ c, scope }: P) {
  const t = str(resolve(c.text, scope)), v = String(c.variant ?? "body")
  if (/^h[1-5]$/.test(v)) { const H = (v === "h1" ? "h3" : "h4") as "h3"; return <H className={cn("text-foreground", v === "h1" || v === "h2" ? "type-headline" : "type-body font-semibold")} data-a2ui-text={v}>{t}</H> }
  if (v === "caption") return <div className="type-footnote text-muted-foreground" data-a2ui-text={v}>{t}</div>
  return MD.test(t) ? <Markdown text={t} className="type-body" /> : <span className="block type-body" data-a2ui-text={v}>{t}</span>
}
const safeMedia = (u: string, image = false) => /^https:\/\//i.test(u) || (image && /^data:image\/(png|jpe?g|gif|webp);/i.test(u))
const IMG: Record<string, string> = { icon: "size-6", avatar: "size-10 rounded-full", smallFeature: "max-h-32 max-w-48", mediumFeature: "max-h-60 max-w-full", largeFeature: "max-h-96 max-w-full", header: "h-40 w-full" }
const FIT: Record<string, string> = { contain: "object-contain", cover: "object-cover", fill: "object-fill", none: "object-none", scaleDown: "object-scale-down" }
function Image({ c, scope }: P) {
  const t = useT(), u = str(resolve(c.url, scope))
  if (!safeMedia(u, true)) return <Unsupported what={t("misc:a2ui.imageHttps")} />
  return <img src={u} alt={str(resolve(c.description, scope)) || a11y(c, scope) || ""} loading="lazy" referrerPolicy="no-referrer" className={cn("rounded-lg", IMG[String(c.variant)] ?? IMG.mediumFeature, FIT[String(c.fit)] ?? (c.variant === "header" ? "object-cover" : "object-contain"))} />
}
function Media({ c, scope }: P) {
  const t = useT(), u = str(resolve(c.url, scope))
  if (!safeMedia(u)) return <Unsupported what={t("misc:a2ui.mediaHttps")} />
  return c.component === "Video" ? <video src={u} controls preload="metadata" className="max-h-80 w-full rounded-lg bg-black" /> : <audio src={u} controls preload="metadata" className="w-full" aria-label={str(resolve(c.description, scope)) || undefined} />
}
const ICONS: Record<string, ComponentType<{ className?: string }>> = {
  accountCircle: L.CircleUserIcon, add: L.PlusIcon, arrowBack: L.ArrowLeftIcon, arrowForward: L.ArrowRightIcon, attachFile: L.PaperclipIcon, calendarToday: L.CalendarIcon,
  call: L.PhoneCallIcon, camera: L.CameraIcon, check: L.CheckIcon, close: L.XIcon, delete: L.Trash2Icon, download: L.DownloadIcon, edit: L.PencilIcon, event: L.CalendarDaysIcon,
  error: L.CircleAlertIcon, fastForward: L.FastForwardIcon, favorite: L.HeartIcon, favoriteOff: L.HeartOffIcon, folder: L.FolderIcon, help: L.CircleHelpIcon, home: L.HouseIcon,
  info: L.InfoIcon, locationOn: L.MapPinIcon, lock: L.LockIcon, lockOpen: L.LockOpenIcon, mail: L.MailIcon, menu: L.MenuIcon, moreVert: L.EllipsisVerticalIcon, moreHoriz: L.EllipsisIcon,
  notificationsOff: L.BellOffIcon, notifications: L.BellIcon, pause: L.PauseIcon, payment: L.CreditCardIcon, person: L.UserIcon, phone: L.PhoneIcon, photo: L.ImageIcon, play: L.PlayIcon,
  print: L.PrinterIcon, refresh: L.RefreshCwIcon, rewind: L.RewindIcon, search: L.SearchIcon, send: L.SendIcon, settings: L.SettingsIcon, share: L.Share2Icon, shoppingCart: L.ShoppingCartIcon,
  skipNext: L.SkipForwardIcon, skipPrevious: L.SkipBackIcon, star: L.StarIcon, starHalf: L.StarHalfIcon, starOff: L.StarOffIcon, stop: L.SquareIcon, upload: L.UploadIcon,
  visibility: L.EyeIcon, visibilityOff: L.EyeOffIcon, volumeDown: L.Volume1Icon, volumeMute: L.VolumeXIcon, volumeOff: L.VolumeOffIcon, volumeUp: L.Volume2Icon, warning: L.TriangleAlertIcon,
}
function Icon({ c, scope }: P) {
  const n = resolve(c.name, scope)
  if (n && typeof n === "object" && typeof (n as { svgPath?: unknown }).svgPath === "string")
    return <svg viewBox="0 0 24 24" className="size-4 shrink-0 fill-current text-muted-foreground" aria-label={a11y(c, scope)} aria-hidden={!a11y(c, scope)}><path d={(n as { svgPath: string }).svgPath} /></svg>
  const I = ICONS[str(n)]
  return I ? <I className="size-4 shrink-0 text-muted-foreground" /> : null
}
function Row(p: P) {
  return <div className={cn("flex min-w-0 flex-wrap gap-3", JUSTIFY[String(p.c.justify)] ?? "justify-start", ALIGN[String(p.c.align)] ?? "items-center")} aria-label={a11y(p.c, p.scope)}><Kids {...p} /></div>
}
function Column(p: P) {
  return <div className={cn("flex min-w-0 flex-col gap-3", JUSTIFY[String(p.c.justify)] ?? "justify-start", ALIGN[String(p.c.align)] ?? "items-stretch")} aria-label={a11y(p.c, p.scope)}><Kids {...p} /></div>
}
function List(p: P) {
  const h = p.c.direction === "horizontal"
  return <div role="list" className={cn("flex min-w-0 gap-2", h ? "flex-row overflow-x-auto pb-1" : "flex-col", ALIGN[String(p.c.align)] ?? "items-stretch")}><Kids {...p} /></div>
}
function Card({ c, scope, depth, trail }: P) {
  return <div className="min-w-0 rounded-xl bg-card p-4 shadow-[var(--lift)]" data-a2ui-card><Child id={c.child} scope={scope} depth={depth} trail={trail} /></div>
}
function TabsC({ c, scope, depth, trail }: P) {
  const t = useT()
  const tabs = Array.isArray(c.tabs) ? (c.tabs as { title?: unknown; child?: unknown }[]).filter((x) => x && typeof x.child === "string") : []
  if (!tabs.length) return null
  return (
    <Tabs defaultValue={0} className="min-w-0">
      <TabsList className="max-w-full overflow-x-auto">{tabs.map((x, i) => <TabsTrigger key={i} value={i}>{str(resolve(x.title, scope)) || t("misc:a2ui.tab", { n: i + 1 })}</TabsTrigger>)}</TabsList>
      {tabs.map((x, i) => <TabsContent key={i} value={i} className="pt-1"><Child id={x.child} scope={scope} depth={depth} trail={trail} /></TabsContent>)}
    </Tabs>
  )
}
function Modal({ c, scope, depth, trail }: P) {
  const t = useT(), [open, setOpen] = useState(false), run = useRun()
  return <>
    <RunCtx.Provider value={{ ...run, trigger: () => setOpen(true) }}><Child id={c.trigger} scope={scope} depth={depth} trail={trail} /></RunCtx.Provider>
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-lg"><DialogTitle className="sr-only">{a11y(c, scope) ?? t("misc:a2ui.details")}</DialogTitle><Child id={c.content} scope={scope} depth={depth} trail={trail} /></DialogContent>
    </Dialog>
  </>
}
const Divider = ({ c }: P) => c.axis === "vertical" ? <div className="w-px self-stretch bg-border" role="separator" aria-orientation="vertical" /> : <hr className="border-border" />

/** the text a button shows, for the “提交了「…」” line */
function labelOf(id: unknown, run: Run, scope: Scope, n = 0): string {
  const c = typeof id === "string" ? run.s.comps.get(id) : undefined
  if (!c || n > 4) return ""
  if (c.component === "Text") return str(resolve(c.text, scope))
  if (typeof c.child === "string") return labelOf(c.child, run, scope, n + 1)
  if (Array.isArray(c.children)) return c.children.map((k) => labelOf(k, run, scope, n + 1)).filter(Boolean).join(" ")
  return ""
}
function ButtonC({ c, scope, depth, trail }: P) {
  const t = useT(), run = useRun(), [errs, setErrs] = useState<string[]>([])
  const ev = !!(c.action as { event?: unknown } | undefined)?.event
  const label = labelOf(c.child, run, scope) || a11y(c, scope) || t("misc:a2ui.button")
  const mine = run.done?.sourceComponentId === c.id
  const click = () => {
    if (run.trigger) { run.trigger(); return }
    const bad = failedChecks(c, scope)
    setErrs(bad)
    if (!bad.length) void run.fire(c, scope, label)
  }
  const v = String(c.variant ?? "default")
  return (
    <div className="flex min-w-0 flex-col items-start gap-1">
      <Button type="button" size="sm" variant={v === "primary" ? "default" : v === "borderless" ? "ghost" : "outline"} className="max-w-full"
        disabled={!run.trigger && ev && (!!run.locked || !!run.busy)} aria-label={a11y(c, scope)} onClick={click} data-a2ui-button={c.id} data-a2ui-submitted={mine || undefined}>
        {run.busy === c.id ? <Spinner className="size-3.5" /> : mine ? <L.CheckIcon /> : null}
        <span className="truncate"><Child id={c.child} scope={scope} depth={depth} trail={trail} /></span>
      </Button>
      {errs.length > 0 && <div role="alert" className="type-footnote text-destructive">{errs.join(t("misc:a2ui.errSep"))}</div>}
    </div>
  )
}

/** a bound input's path (written back on change), or a private slot for a literal value */
function useBound(c: Comp, scope: Scope, prop = "value") {
  const run = useRun(), v = c[prop]
  const path = isBinding(v) ? absolute(v.path, scope.base) : `/$local/${c.id}${scope.base === "/" ? "" : scope.base}`
  const cur = getPointer(run.data, path)
  const value = cur !== undefined ? cur : isBinding(v) ? undefined : resolve(v, scope)
  return { value, set: (x: unknown) => run.set(path, x), off: !!run.locked }
}
function Field({ label, children, errs, htmlFor }: { label?: string; children: ReactNode; errs?: string[]; htmlFor?: string }) {
  const t = useT()
  return (
  <div className="flex min-w-0 flex-col gap-1.5">
    {label && <label htmlFor={htmlFor} className="type-label text-muted-foreground">{label}</label>}
    {children}
    {errs && errs.length > 0 && <div className="type-footnote text-destructive">{errs.join(t("misc:a2ui.errSep"))}</div>}
  </div>
  )
}
function TextField({ c, scope }: P) {
  const t = useT(), b = useBound(c, scope), [touched, setTouched] = useState(false), id = `a2ui-${c.id}-${scope.base}`
  const v = String(c.variant ?? "shortText"), val = str(b.value)
  const re = typeof c.validationRegexp === "string" && c.validationRegexp.length <= 200 ? (() => { try { return new RegExp(c.validationRegexp as string) } catch { return null } })() : null
  const errs = touched ? [...(re && val && !re.test(val) ? [t("misc:a2ui.badFormat")] : []), ...failedChecks(c, scope)] : []
  const props = { id, value: val, disabled: b.off, onBlur: () => setTouched(true), "aria-invalid": errs.length > 0 || undefined }
  return (
    <Field label={str(resolve(c.label, scope))} errs={errs} htmlFor={id}>
      {v === "longText" ? <Textarea {...props} rows={3} onChange={(e) => b.set(e.target.value)} />
        : <Input {...props} type={v === "number" ? "number" : v === "obscured" ? "password" : "text"} onChange={(e) => b.set(v === "number" && e.target.value !== "" ? Number(e.target.value) : e.target.value)} />}
    </Field>
  )
}
function CheckBoxC({ c, scope }: P) {
  const b = useBound(c, scope)
  return (
    <label className="flex min-w-0 items-center gap-2 type-body">
      <Checkbox checked={!!b.value} disabled={b.off} onCheckedChange={(x) => b.set(!!x)} />
      <span className="min-w-0">{str(resolve(c.label, scope))}</span>
    </label>
  )
}
function ChoicePicker({ c, scope }: P) {
  const t = useT(), b = useBound(c, scope), [q, setQ] = useState("")
  const multi = c.variant === "multipleSelection"
  const opts = (Array.isArray(c.options) ? c.options : []).filter((o): o is { label: unknown; value: string } => !!o && typeof (o as { value?: unknown }).value === "string")
    .map((o) => ({ value: o.value, label: str(resolve(o.label, scope)) || o.value }))
  const sel = Array.isArray(b.value) ? b.value.map(String) : b.value != null && b.value !== "" ? [str(b.value)] : []
  const pick = (v: string) => b.set(multi ? (sel.includes(v) ? sel.filter((x) => x !== v) : [...sel, v]) : [v])
  const shown = c.filterable && q ? opts.filter((o) => o.label.toLowerCase().includes(q.toLowerCase())) : opts
  const label = str(resolve(c.label, scope))
  const chips = c.displayStyle === "chips"
  return (
    <Field label={label}>
      {c.filterable === true && opts.length > 6 && <Input value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("misc:a2ui.filter")} className="h-8 max-w-60" />}
      {chips || multi
        ? <div role={multi ? "group" : "radiogroup"} aria-label={label || undefined} className={cn("flex min-w-0 flex-wrap", chips ? "gap-1" : "flex-col gap-2")} data-a2ui-choice={c.id}>
            {shown.map((o) => {
              const on = sel.includes(o.value)
              return chips
                ? <button key={o.value} type="button" role={multi ? "checkbox" : "radio"} aria-checked={on} disabled={b.off} onClick={() => pick(o.value)} data-a2ui-option={o.value}
                    className={cn("h-7 rounded-lg px-2.5 type-callout transition-colors duration-120 disabled:opacity-60", on ? "bg-accent font-medium text-foreground" : "text-muted-foreground hover:bg-accent hover:text-foreground")}>{o.label}</button>
                : <label key={o.value} className="flex items-center gap-2.5 type-body" data-a2ui-option={o.value}>
                    <Checkbox checked={on} disabled={b.off} onCheckedChange={() => pick(o.value)} />{o.label}</label>
            })}
          </div>
        : <RadioGroup value={sel[0] ?? null} onValueChange={(v) => pick(String(v))} disabled={b.off} name={`a2ui-${c.id}-${scope.base}`} aria-label={label || undefined} className="flex min-w-0 flex-col gap-2" data-a2ui-choice={c.id}>
            {shown.map((o) => <label key={o.value} className="flex items-center gap-2.5 type-body" data-a2ui-option={o.value}><RadioGroupItem value={o.value} />{o.label}</label>)}
          </RadioGroup>}
    </Field>
  )
}
function Slider({ c, scope }: P) {
  const b = useBound(c, scope), min = typeof c.min === "number" ? c.min : 0, max = typeof c.max === "number" ? c.max : 100
  const v = typeof b.value === "number" ? b.value : Number(b.value ?? min)
  return (
    <Field label={str(resolve(c.label, scope))}>
      <div className="flex items-center gap-3">
        <input type="range" min={min} max={max} step={max - min <= 10 && !Number.isInteger(max - min) ? 0.1 : 1} value={Number.isFinite(v) ? v : min} disabled={b.off} onChange={(e) => b.set(Number(e.target.value))} className="min-w-0 flex-1 accent-primary" />
        <span className="w-12 shrink-0 text-right type-callout tabular-nums">{Number.isFinite(v) ? v : min}</span>
      </div>
    </Field>
  )
}
function DateTimeInput({ c, scope }: P) {
  const b = useBound(c, scope), d = c.enableDate !== false && (c.enableDate === true || c.enableTime !== true), t = c.enableTime === true
  const type = d && t ? "datetime-local" : t ? "time" : "date"
  const v = str(b.value), shown = type === "datetime-local" ? v.slice(0, 16) : type === "date" ? v.slice(0, 10) : v.slice(0, 5)
  return (
    <Field label={str(resolve(c.label, scope))}>
      <Input type={type} value={shown} disabled={b.off} min={str(resolve(c.min, scope)) || undefined} max={str(resolve(c.max, scope)) || undefined} onChange={(e) => b.set(e.target.value)} className="w-auto max-w-full" />
    </Field>
  )
}

/* Steve catalog: a chart over rows of the data model (docs/ST-46-a2ui.md §目录) */
const KIND = { line: "line", bar: "group", stack: "stack" } as const
const DAY = /^\d{4}-\d{2}-\d{2}$/
function ChartC({ c, scope }: P) {
  const t = useT(), [table, setTable] = useState(false)
  const rows = chartRows(resolve(c.data, scope), resolve(c.last, scope))
  const x = typeof c.x === "string" ? c.x : "x"
  const series = (Array.isArray(c.series) ? c.series : []).filter((s): s is { key: string; label?: unknown } => !!s && typeof (s as { key?: unknown }).key === "string")
    .map((s) => ({ key: s.key, label: str(resolve(s.label, scope)) || s.key }))
  const unit = str(resolve(c.unit, scope)), title = str(resolve(c.title, scope))
  const fmt = (v: number) => `${num(v, { maximumFractionDigits: 2 })}${unit && unit.length <= 2 ? unit : ""}`
  const n = (v: unknown) => (typeof v === "number" && Number.isFinite(v) ? v : typeof v === "string" && v.trim() !== "" && Number.isFinite(Number(v)) ? Number(v) : null)
  const kind = String(c.kind ?? "line")
  if (!series.length || !rows.length) return <Unsupported what={title ? t("misc:a2ui.noDataTitled", { title }) : t("misc:a2ui.noData")} />
  const legend = series.length > 1 ? series.map((s, i) => ({ label: s.label, c: seriesColor(i), line: kind === "line" })) : undefined
  const head = <ChartHead title={title || series[0].label} meta={[unit, rows.length > 1 && DAY.test(str(rows[0][x])) ? t("common:time.range", { from: str(rows[0][x]), to: str(rows[rows.length - 1][x]) }) : ""].filter(Boolean).join(" · ") || undefined} legend={legend}
    right={<button type="button" className="type-footnote text-muted-foreground hover:text-foreground" aria-pressed={table} onClick={() => setTable(!table)} data-a2ui-chart-table>{table ? t("misc:a2ui.showChart") : t("misc:a2ui.showTable")}</button>} />
  const body = table ? (
    <div className="md" data-md-table><div className="overflow-x-auto"><table><thead><tr><th>{x}</th>{series.map((s) => <th key={s.key}>{s.label}</th>)}</tr></thead>
      <tbody>{rows.map((r, i) => <tr key={i}><td>{str(r[x])}</td>{series.map((s) => <td key={s.key} className="tabular-nums">{n(r[s.key]) == null ? "—" : fmt(n(r[s.key])!)}</td>)}</tr>)}</tbody></table></div></div>
  ) : kind === "hbar" ? (
    <HBars label={title || series[0].label} fmt={fmt} vw="w-20" rows={rows.map((r, i) => ({ key: `${i}`, label: str(r[x]), value: n(r[series[0].key]) ?? 0, c: seriesColor(0), title: str(r[x]) }))} />
  ) : (
    <Chart kind={KIND[kind as keyof typeof KIND] ?? "line"} label={title || series[0].label} fmt={fmt} height={typeof c.height === "number" ? Math.min(400, Math.max(140, c.height)) : 200}
      xFmt={(d) => (DAY.test(d) ? shortDay(d) : d.length > 8 ? d.slice(0, 7) + "…" : d)}
      data={{ buckets: rows.map((r) => str(r[x])), series: series.map((s) => ({ key: s.key, label: s.label, values: rows.map((r) => n(r[s.key])) })) }} />
  )
  return <div className="min-w-0" data-a2ui-chart={kind}>{head}{body}</div>
}

const BASIC_COMPONENTS: Record<string, ComponentType<P>> = {
  Text, Image, Icon, Video: Media, AudioPlayer: Media, Row, Column, List, Card, Tabs: TabsC, Modal, Divider,
  Button: ButtonC, TextField, CheckBox: CheckBoxC, ChoicePicker, Slider, DateTimeInput,
}
const STEVE_COMPONENTS: Record<string, ComponentType<P>> = { ...BASIC_COMPONENTS, Chart: ChartC }
