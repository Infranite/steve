import { getItems, useDeviceInventory } from "@/store"
/* 工作空间与项目：工作空间是数据边界，切换后所有页面只显示这个空间的内容；一个工作空间管理多个项目，每个项目一个独立目录 */
import { api, type CloneProgress, type RBrowse, type RNode, type RProject } from "@/api"
import { SegmentedControl } from "@/components/affect/segmented-control-clean"
import { stats, statsSeed } from "@/api/stats"
import type { GhOverview } from "@/api/github"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { Switch } from "@/components/ui/switch"
import { Textarea } from "@/components/ui/textarea"
import { AskSteve } from "@/config-ui"
import { crumbs, emptyNote, lookUp, nameOf, narrowed, projectNodes } from "@/dir-browse"
import { GitHubLine, Health, projectRepo, useProjectGitHub } from "@/github/index"
import { itemDisplayStatus } from "@/store"
import { fmtMs, fmtN, fmtPct, fmtUsd, useStats } from "@/stats/format"
import { botName, closedSt, newProject, newWorkspace, patchProject, refresh, st, toast, type Item } from "@/store"
import { Blank, BotName, Card, closeSheet, Empty, Fact, Figure, go, Group, Loading, Meta, openSheet, Page, Panel, Row, Section, Segmented } from "@/ui"
import { ItemRowExport as ItemRow, Select } from "@/view-shared"
import { cn } from "cn"
import { ArchiveIcon, ChevronRightIcon, CornerLeftUpIcon, FolderGit2Icon, FolderIcon, PlusIcon, SparklesIcon } from "lucide-react"
import { Fragment, useEffect, useRef, useState } from "react"
import { useT } from "@/i18n"

const home = (p: string) => p.replace(/^\/Users\/[^/]+/, "~")
export const projectIcon = (kind: string) => (kind === "git" ? FolderGit2Icon : FolderIcon)
export function NewWorkspaceDialog() {
  const t = useT()
  const [n, setN] = useState(""), [d, setD] = useState(""), [p, setP] = useState("")
  const prefix = p.trim().toUpperCase().replace(/[^A-Z]/g, "").slice(0, 6)
  return (
    <Dialog open onOpenChange={(o) => !o && closeSheet()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[480px]">
        <DialogHeader><DialogTitle className="type-headline">{t("pages:projects.newSpace")}</DialogTitle><DialogDescription className="type-callout">{t("pages:projects.newSpaceDesc")}</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4">
          <Field><FieldLabel htmlFor="ws-n">{t("pages:projects.name")}</FieldLabel><Input id="ws-n" autoFocus className="rounded-lg" value={n} onChange={(e) => setN(e.target.value)} placeholder={t("pages:projects.spaceNamePh")} /></Field>
          <Field><FieldLabel htmlFor="ws-d">{t("pages:projects.desc")}</FieldLabel><Textarea id="ws-d" className="min-h-16 rounded-lg" value={d} onChange={(e) => setD(e.target.value)} placeholder={t("pages:projects.spaceDescPh")} /></Field>
          <Field><FieldLabel htmlFor="ws-p">{t("pages:projects.prefix")}</FieldLabel><Input id="ws-p" className="w-28 rounded-lg font-mono uppercase" value={p} onChange={(e) => setP(e.target.value)} placeholder="ST" maxLength={6} />
            <FieldDescription className="type-footnote">{t("pages:projects.prefixHint", { prefix: prefix || "ST" })}</FieldDescription></Field>
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={closeSheet}>{t("common:action.cancel")}</Button><Button disabled={!n.trim()} onClick={() => { closeSheet(); void newWorkspace(n.trim(), d.trim(), prefix || undefined) }} data-testid="ws-go">{t("pages:projects.createSwitch")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/* ── 项目 ── */
export function ProjectsView() {
  const t = useT()
  const ps = st.projects, running = (id: string) => getItems().filter((i) => i.project === id && ["run", "wait", "block"].includes(itemDisplayStatus(i)) && !i.arch).length
  return (
    <Page title={t("pages:projects.title")} sub={t("pages:projects.sub", { space: st.workspace?.name })} max={720}
      actions={<Button size="sm" onClick={() => openSheet("newProject")} data-new-project><PlusIcon />{t("pages:projects.newProject")}</Button>}>
      {ps.length ? <Group>{ps.map((p) => { const I = projectIcon(p.kind); return (
        <Row key={p.id} icon={<span className="flex size-9 items-center justify-center rounded-lg bg-muted text-muted-foreground"><I className="size-4.5" strokeWidth={1.5} /></span>} onClick={() => go("#/projects/" + p.id)} data-project-row={p.name}
          title={<span className="type-body font-medium">{p.name}</span>}
          sub={<Meta><span className="font-mono">{home(p.path)}</span><span>{p.kind === "git" ? (p.worktree ? t("pages:projects.gitWorktree") : "git") : t("pages:projects.plainDir")}</span>{p.default_bot && <span>{t("pages:projects.defaultBotIs", { bot: botName(p.default_bot) })}</span>}<span>{t("pages:projects.running", { count: running(p.id) })}</span>{p.release && <span>{t("pages:projects.releasable")}</span>}</Meta>}>
          <ChevronRightIcon className="size-4 text-tertiary" /></Row>) })}</Group>
        : <Empty title={t("pages:projects.empty")} sub={t("pages:projects.emptySub")} />}
      <div className="mt-4 type-footnote text-tertiary">{t("pages:projects.hint")}</div>
    </Page>
  )
}

/* A project's page, in the order someone opens it for (Lee, 2026-11-13: "信息堆叠没有主次"):
 *  概览  1. 事项: what is going on now — waiting on you first, then what failed or is stuck, then what runs, then the
 *           latest finished; a merge batch in flight under them. One line when there is nothing.
 *        2. 最近 7 天: how it is doing — done, landed and released, failures, cost, Agent time, your time
 *           (GET /stats/summary?project=).
 *        3. 仓库与交付: where it lives and how work lands, one fact a line; under it, folded, the conventions Steve
 *           found in the repository (checks, lock files, migrations, release notes), each listed once.
 *  设置  telling Steve, name and description, how its work runs, the GitHub link. */
type Tab = "overview" | "settings"
let lastTab: { id: string; tab: Tab } = { id: "", tab: "overview" }
const RECENT = 6
/* the order of open items: your move first, then what failed or is stuck, then the paused, then what runs */
const RANK: Partial<Record<Item["st"], number>> = { wait: 0, prop: 0, fail: 1, block: 1, pause: 2 }
/** a project's items for its page: the open ones in that order, then the latest finished up to six in all */
export function projectItems(project: string, items: Item[]) {
  const mine = items.filter((i) => i.project === project && !i.arch)
  const open = mine.filter((i) => !closedSt(i.st)).map((i) => ({ i, r: RANK[itemDisplayStatus(i)] ?? 3 })).sort((a, b) => a.r - b.r || b.i.updated - a.i.updated)
  const closed = mine.filter((i) => closedSt(i.st)).sort((a, b) => b.updated - a.updated)
  return { open: open.length, yours: open.filter((x) => x.r === 0).length, list: [...open.map((x) => x.i), ...closed.slice(0, Math.max(0, RECENT - open.length))] }
}

export function ProjectView({ id }: { id: string }) {
  const t = useT()
  const p = st.projects.find((x) => x.id === id)
  const [tab, setTab0] = useState<Tab>(lastTab.id === id ? lastTab.tab : "overview")
  const setTab = (v: Tab) => { lastTab = { id, tab: v }; setTab0(v) }
  const [gh, setGh, ghError] = useProjectGitHub(p?.kind === "git" ? id : undefined)
  if (!p) return <Page title={t("pages:projects.title")} back={["#/projects", t("pages:projects.title")]}><Empty title={t("pages:projects.notFound")} sub={t("pages:projects.notFoundSub")} /></Page>
  return (
    <Page title={p.name} back={["#/projects", t("pages:projects.title")]} max={720} sub={p.description ? <span className="line-clamp-2" data-proj-desc>{p.description}</span> : undefined}
      actions={<Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => { patchProject(p.id, { archived: !p.archived }, p.archived ? t("pages:projects.restored") : t("pages:projects.archived")); if (!p.archived) go("#/projects") }}><ArchiveIcon />{p.archived ? t("pages:projects.restore") : t("pages:projects.archive")}</Button>}>
      <div className="mb-8"><Segmented value={tab} onChange={setTab} label={p.name} options={[["overview", t("pages:projects.tabOverview")], ["settings", t("pages:projects.settings")]] as const} /></div>
      {tab === "overview"
        ? <Overview p={p} gh={gh} ghError={ghError} onSettings={() => setTab("settings")} />
        : <Settings p={p} gh={gh} ghError={ghError} onGh={setGh} />}
    </Page>
  )
}

type Parts = { p: RProject; gh: GhOverview | null; ghError: string }

function Overview({ p, gh, ghError, onSettings }: Parts & { onSettings: () => void }) {
  const t = useT()
  const { open, yours, list } = projectItems(p.id, getItems())
  return (
    <div className="flex flex-col gap-8 anim-in" data-proj-overview>
      <Section label={t("pages:projects.items")} data-proj-items
        right={open > 0 ? <Meta inline className="font-normal">{yours > 0 && <span className="text-attention" data-proj-yours>{t("pages:projects.yours", { count: yours })}</span>}<span>{t("pages:projects.running", { count: open })}</span></Meta> : undefined}>
        {list.length ? <Panel>{list.map((i) => <ItemRow key={i.id} i={i} proj={false} />)}</Panel> : <Blank data-proj-none>{t("pages:projects.noItems")}</Blank>}
      </Section>
      <Recent p={p} />
      <Where p={p} gh={gh} ghError={ghError} onSettings={onSettings} />
    </div>
  )
}

/** the last 7 days in six figures: what got done, landed and released, what failed, and what it cost in money and time */
function Recent({ p }: { p: RProject }) {
  const t = useT()
  const [d] = useStats(() => stats.summary({ range: "7d", project: p.id }), [p.id])
  const s = d.s === "ok" ? d.v : null
  const fails = s ? Object.entries(s.quality.fails).filter(([, n]) => n > 0).sort((a, b) => b[1] - a[1]) : []
  const figs = [
    s && { k: "done", label: t("pages:projects.fig.done"), value: fmtN(s.throughput.done), sub: s.throughput.wip ? t("pages:projects.fig.wip", { count: s.throughput.wip }) : undefined },
    s && { k: "fails", label: t("pages:projects.fig.fails"), value: fmtN(fails.reduce((a, [, n]) => a + n, 0)), sub: fails.slice(0, 2).map(([k, n]) => `${t(`stats:quality.fail.${k}` as "stats:quality.fail.other")} ${n}`).join(" · ") || undefined },
    s && { k: "cost", label: t("pages:projects.fig.cost"), value: s.spend.est > 0 ? fmtUsd(s.spend.est) : s.spend.reported > 0 ? "≥" + fmtUsd(s.spend.reported) : "—", sub: s.per_item.est_avg ? t("pages:projects.fig.perItem", { v: fmtUsd(s.per_item.est_avg) }) : undefined },
    s && { k: "agent", label: t("pages:projects.fig.agent"), value: s.usage.turns ? fmtMs(s.usage.turn_ms) : "—", sub: s.usage.turns ? t("pages:projects.fig.turns", { count: s.usage.turns }) : undefined },
    s && { k: "wait", label: t("pages:projects.fig.wait"), value: s.time.wait.n ? fmtMs(s.time.wait.sum) : "—", sub: s.human.decisions ? t("pages:projects.fig.decisions", { count: s.human.decisions }) : undefined },
    s && s.quality.first_pass_n > 0 && { k: "first-pass", label: t("manage:ov.firstPass"), value: fmtPct(s.quality.first_pass), sub: t("manage:ov.ofDelivered", { n: s.quality.first_pass_n }) },
  ].filter((x): x is { k: string; label: string; value: string; sub: string | undefined } => !!x)
  const quiet = (!s || (!s.throughput.created && !s.throughput.done && !s.throughput.wip && !s.usage.turns))
  const openStats = () => { statsSeed.next = { range: "7d", project: p.id }; go("#/stats") }
  return (
    <Section label={t("pages:projects.recent")} data-proj-recent
      right={s && !quiet ? <button type="button" onClick={openStats} className="type-footnote font-normal text-muted-foreground hover:text-foreground" data-proj-stats-open>{t("pages:projects.openStats")}</button> : undefined}>
      {d.s === "loading" ? <Loading className="py-8" />
        : quiet ? <Blank data-proj-quiet>{d.s === "error" ? d.msg : t("pages:projects.quiet")}</Blank>
        : <Card className="grid grid-cols-2 gap-0 p-1 sm:grid-cols-3" data-proj-figures>{figs.map((f) => <Figure key={f.k} testId={f.k} label={f.label} value={f.value} sub={f.sub} />)}</Card>}
    </Section>
  )
}

/** where the project lives and how its work lands, one fact a line; the repository's conventions folded under them */
function Where({ p, gh, ghError, onSettings }: Parts & { onSettings: () => void }) {
  const t = useT()
  const git = p.kind === "git"
  const device = useDeviceInventory().find((x) => x.id === p.device_id)?.name ?? "Hub"
  const repo = gh ? projectRepo(gh, p.id) : undefined
  const code = "font-mono text-[13px] break-all"
  const cmd = p.release?.cmd ? <span className={cn("mt-0.5 block text-muted-foreground", code)}>{home(p.release.cmd)}</span> : null
  const facts = [
    <Fact key="dir" label={t("pages:projects.dir")} testId="dir">
      <span className={cn("block", code)}>{home(p.path)}</span>
      <Meta className="mt-0.5"><span>{git ? (p.worktree ? t("pages:projects.gitWorktree") : t("pages:projects.gitRepo")) : t("pages:projects.plainDir")}</span><span>{t("pages:projects.onDevice", { device })}</span></Meta>
    </Fact>,
    git && ghError && <Fact key="gh-error" label="GitHub" testId="github-error"><span role="alert" className="text-destructive">{ghError}</span></Fact>,
    git && gh && (repo
      ? <Fact key="gh" label="GitHub" testId="github" onClick={() => go("#/github/" + repo.id)}>{repo.full_name}<Meta className="mt-0.5"><Health r={repo} /><span>{t("github:shared.openPrs", { count: repo.open_prs })}</span></Meta></Fact>
      : <Fact key="gh" label="GitHub" testId="github" onClick={onSettings}><span className="text-muted-foreground">{t("pages:projects.ghNone")}</span></Fact>),
    <Fact key="ship" label={t("pages:projects.release")} testId="ship">
      {cmd ?? <span className="text-muted-foreground">{t("pages:projects.releaseNone")}</span>}
    </Fact>,
  ]
  return (
    <Section label={git ? t("pages:projects.where") : t("pages:projects.whereDir")} data-proj-where>
      <Panel>{facts}</Panel>
    </Section>
  )
}

function Settings({ p, gh, ghError, onGh }: Parts & { onGh: (v: GhOverview) => void }) {
  const t = useT()
  const [n, setN] = useState(p.name), [d, setD] = useState(p.description)
  useEffect(() => { setN(p.name); setD(p.description) }, [p.updated])
  const dirty = n.trim() !== p.name || d.trim() !== p.description
  return (
    <div className="flex flex-col gap-8 anim-in" data-proj-settings>
      <AskSteve key={p.id} ctx={{ kind: "project", id: p.id }} placeholder={t("pages:projects.askPh")} />
      <Section label={t("pages:projects.basics")}>
        <FieldGroup className="gap-4 px-1">
          <Field className="gap-1.5"><FieldLabel className="type-label text-tertiary">{t("pages:projects.name")}</FieldLabel><Input className="rounded-lg" value={n} onChange={(e) => setN(e.target.value)} data-proj-name /></Field>
          <Field className="gap-1.5"><FieldLabel className="type-label text-tertiary">{t("pages:projects.desc")}</FieldLabel><Textarea className="min-h-20 rounded-lg" value={d} onChange={(e) => setD(e.target.value)} />
            <FieldDescription className="type-footnote">{t("pages:projects.descHint")}</FieldDescription></Field>
          {dirty && <div className="flex gap-2"><Button size="sm" onClick={() => patchProject(p.id, { name: n.trim() || p.name, description: d.trim() })}>{t("common:action.save")}</Button><Button size="sm" variant="ghost" onClick={() => { setN(p.name); setD(p.description) }}>{t("common:action.cancel")}</Button></div>}
        </FieldGroup>
      </Section>
      <Section label={t("pages:projects.work")}>
        <Panel>
          <Row title={t("pages:projects.defaultBot")} sub={t("pages:projects.defaultBotSub")}><Select align="end" testId="proj-bot" value={p.default_bot ?? "none"} onChange={(v) => patchProject(p.id, { default_bot: v === "none" ? null : v })} options={[{ v: "none", l: t("pages:projects.bySteve") }, ...Object.keys(st.bots).filter((k) => st.bots[k].on).map((k) => ({ v: k, l: <BotName k={k} /> }))]} /></Row>
          {p.kind === "git" && <Row title={t("pages:projects.worktree")} sub={t("pages:projects.worktreeSub")}><Switch checked={p.worktree} onCheckedChange={(v) => patchProject(p.id, { worktree: v })} data-proj-worktree /></Row>}
        </Panel>
      </Section>
      {p.kind === "git" && <Section label="GitHub">
        {ghError ? <p role="alert" className="type-callout text-destructive">{ghError}</p> : gh ? <Panel><GitHubLine project={p.id} ov={gh} onOv={onGh} /></Panel> : <Loading />}
      </Section>}
    </div>
  )
}

/** the directory chooser of a new project: the directories of where `path` is on the Node's machine; a click goes
 *  into one, and the directory you are in is the project's. A path still being typed narrows its parent's list */
function DirBrowser({ path, node, onPick, onSeen }: { path: string; node: string; onPick: (p: string) => void; onSeen: (git: boolean | null) => void }) {
  const t = useT()
  const [at, setAt] = useState<RBrowse | null>(null), [typed, setTyped] = useState(""), [err, setErr] = useState("")
  // a click goes there at once, so the list under the pointer is never the one just left; typing waits for a pause
  const clicked = useRef(false), go = (p: string) => { clicked.current = true; onPick(p) }
  useEffect(() => {
    let live = true
    const timer = setTimeout(async () => {
      try {
        const seen = await lookUp((p) => api.browse(p, node), path)
        if (!live) return
        setAt(seen.at); setTyped(seen.typed); setErr(""); onSeen(seen.git)
      } catch (e) { if (live) { setAt(null); setErr((e as Error).message) } }
    }, at && !clicked.current ? 150 : 0)
    clicked.current = false
    return () => { live = false; clearTimeout(timer) }
  }, [path, node])
  if (err) return <FieldDescription className="type-footnote" data-dir-browser-error>{err}</FieldDescription>
  if (!at) return <div className="flex h-24 items-center justify-center rounded-lg border border-border"><Spinner className="size-4" /></div>
  const cs = crumbs(at.path), abs = at.path.startsWith("/"), shown = narrowed(at, typed), why = emptyNote(at, typed)
  const note = why ? t(`pages:projects.${why}`, { text: typed }) : ""
  return (
    <div className="overflow-hidden rounded-lg border border-border" data-dir-browser>
      <div className="flex items-center gap-1 border-b border-border bg-muted/40 py-1 pr-2.5 pl-1">
        <Button type="button" size="icon-xs" variant="ghost" className="shrink-0 text-muted-foreground" disabled={at.parent == null} onClick={() => at.parent != null && go(at.parent)} title={t("pages:projects.browseUp")} data-dir-up><CornerLeftUpIcon /></Button>
        <div className="flex min-w-0 flex-1 items-center overflow-x-auto whitespace-nowrap type-code text-muted-foreground">
          {cs.map((c, i) => <Fragment key={c.path}>{i > (abs ? 1 : 0) && <span className="px-0.5 text-tertiary">/</span>}
            <button type="button" onClick={() => go(c.path)} className={cn("rounded-md px-1 py-0.5 hover:bg-accent hover:text-foreground", i === cs.length - 1 && !typed && "text-foreground")}>{c.name}</button></Fragment>)}
        </div>
        {at.git && !typed && <span className="shrink-0 rounded-md bg-background px-1.5 py-0.5 type-label text-muted-foreground">git</span>}
      </div>
      <div className="h-44 overflow-y-auto p-1">
        {note ? <div className="px-3 py-4 text-center type-footnote text-muted-foreground">{note}</div>
          : shown.map((x) => { const I = x.git ? FolderGit2Icon : FolderIcon; return (
            <button key={x.path} type="button" onClick={() => go(x.path)} className="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-left type-callout hover:bg-accent" data-dir={x.name}>
              <I className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.5} /><span className="min-w-0 flex-1 truncate">{x.name}</span>
              {x.git && <span className="type-footnote text-muted-foreground">git</span>}<ChevronRightIcon className="size-3.5 shrink-0 text-tertiary" /></button>) })}
        {at.more && !note && <div className="px-3 py-1.5 type-footnote text-muted-foreground">{t("pages:projects.browseMore", { count: at.dirs.length })}</div>}
      </div>
    </div>
  )
}

type ProjectMode = "existing" | "empty" | "clone"
type TargetCheck = { key: string; path: string } | { key: string; error: string }

export function NewProjectDialog() {
  const t = useT()
  const [path, setPath] = useState(""), [n, setN] = useState(""), [d, setD] = useState(""), [bot, setBot] = useState("none"), [seenGit, setSeenGit] = useState<boolean | null>(null), [wt, setWt] = useState(true)
  const [mode, setMode] = useState<ProjectMode>("existing")
  const [directory, setDirectory] = useState(""), [remoteUrl, setRemoteUrl] = useState(""), [branch, setBranch] = useState("")
  const [named, setNamed] = useState(false), [drafting, setDrafting] = useState(false), [submitting, setSubmitting] = useState(false)
  const [nodes, setNodes] = useState<RNode[]>([]), [progress, setProgress] = useState<CloneProgress | null>(null)
  const [check, setCheck] = useState<TargetCheck>(), [revision, recheck] = useState(0), [error, setError] = useState("")
  const locked = useRef(false), mounted = useRef(false)
  const existing = mode === "existing", clone = mode === "clone", git = clone || (existing && seenGit)
  const parent = path.trim(), dir = directory.trim()
  const checkKey = JSON.stringify([parent, dir, revision])
  const target = check?.key === checkKey ? check : undefined
  const preview = target && "path" in target ? target.path : undefined

  // The Nodes are the owner's to list: anyone else is offered the Hub's own.
  useEffect(() => {
    mounted.current = true
    api.nodes().then((r) => setNodes(r.nodes)).catch(() => {})
    return () => { mounted.current = false }
  }, [])
  useEffect(() => {
    if (existing || !parent || !dir) return
    const abort = new AbortController()
    const timer = setTimeout(async () => {
      try {
        const answer = await api.projectDestination({ parent, directory: dir, node: "hub" }, AbortSignal.any([abort.signal, AbortSignal.timeout(15_000)]))
        if (!abort.signal.aborted) setCheck({ key: checkKey, path: answer.path })
      } catch (e) {
        if (!abort.signal.aborted) setCheck({ key: checkKey, error: (e as Error).message })
      }
    }, 180)
    return () => { clearTimeout(timer); abort.abort() }
  }, [existing, parent, dir, checkKey])

  const { local, remote } = projectNodes(nodes)
  const devices = useDeviceInventory(!local)
  const here = local?.name ?? t("pages:projects.nodeBuiltin", { name: devices.find((x) => x.roles.includes("Hub"))?.name ?? "Hub" })
  const nodeOptions = [{ v: "hub", l: here }, ...remote.map((x) => ({ v: x.node_id, l: t("pages:projects.nodeRemoteSoon", { name: x.name }), disabled: true }))]
  const pick = (p: string) => {
    setPath(p); setSeenGit(null); setError("")
    if (existing && !named) setN(nameOf(p.replace(/\/+$/, "")))
  }
  const changeMode = (m: ProjectMode) => {
    setMode(m); setError("")
    if (!named) setN(m === "existing" ? nameOf(parent.replace(/\/+$/, "")) : dir.slice(0, 40))
  }
  const draft = async () => {
    if (!parent) return
    setDrafting(true)
    try { const r = await api.describeProject(parent); if (!n) setN(r.name); setD(r.description); setSeenGit(r.git) } catch (e) { toast((e as Error).message) } finally { setDrafting(false) }
  }
  const ready = !!n.trim() && (existing ? !!parent : !!preview && (!clone || !!remoteUrl.trim()))
  const submit = async () => {
    if (locked.current || !ready) return
    locked.current = true
    setSubmitting(true); setProgress(null); setError("")
    const fields = {
      name: n.trim(), description: d.trim(), worktree: wt, default_bot: bot === "none" ? null : bot,
      node: "hub" as const,
    }
    try {
      let id
      if (existing) {
        id = await newProject({ ...fields, path: parent })
      } else if (preview) {
        const dest = { ...fields, parent, directory: dir, preview }
        const result = await api.setupProject(clone
          ? { ...dest, mode: "clone", remote: remoteUrl.trim(), branch: branch.trim() || undefined }
          : { ...dest, mode: "empty" }, setProgress)
        id = result.id
        toast(t("core:toast.projectAdded", { name: result.name }))
        await refresh()
      }
      if (id && mounted.current) { closeSheet(); go("#/projects/" + id) }
    } catch (e) {
      setError((e as Error).message)
    } finally {
      locked.current = false
      setSubmitting(false)
    }
  }
  return (
    <Dialog open onOpenChange={(o) => !o && !locked.current && closeSheet()}>
      <DialogContent className="max-h-[92vh] gap-4 overflow-y-auto rounded-2xl sm:max-w-[560px]" showCloseButton={!submitting} data-new-project-dialog>
        <DialogHeader><DialogTitle className="type-headline">{t("pages:projects.newProject")}</DialogTitle><DialogDescription className="type-callout">{t("pages:projects.newProjectDesc")}</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4" inert={submitting || drafting}>
          <Field className="gap-1.5"><div className="flex items-center justify-between gap-3"><FieldLabel>{t("pages:projects.node")}</FieldLabel>
            <Select align="end" testId="project-node" value="hub" onChange={() => {}} options={nodeOptions} /></div>
            {nodeOptions.length > 1 && <FieldDescription className="type-footnote">{t("pages:projects.nodeHint")}</FieldDescription>}</Field>
          <Field className="gap-1.5">
            <FieldLabel id="pj-mode">{t("pages:projects.setupMode")}</FieldLabel>
            <SegmentedControl aria-labelledby="pj-mode" variant="tray" fullWidth value={mode} onChange={changeMode} options={
              (["existing", "empty", "clone"] as const).map(value => ({ value, label: t(`pages:projects.setupModes.${value}`), disabled: value !== "existing" && !st.owner, data: { "data-project-mode": value } }))
            } />
            {!st.owner && <FieldDescription className="type-footnote">{t("pages:projects.setupOwnerOnly")}</FieldDescription>}
          </Field>
          {clone && <>
            <Field className="gap-1.5"><FieldLabel htmlFor="pj-remote">{t("pages:projects.cloneRemote")}</FieldLabel>
              <Input id="pj-remote" className="rounded-lg type-code" autoComplete="off" autoCapitalize="off" spellCheck={false} value={remoteUrl} onChange={(e) => { setRemoteUrl(e.target.value); setError("") }} placeholder="https://example.com/org/repo.git" />
              <FieldDescription className="type-footnote">{t("pages:projects.cloneHint")}</FieldDescription></Field>
            <Field className="gap-1.5"><FieldLabel htmlFor="pj-branch">{t("pages:projects.cloneBranch")}</FieldLabel>
              <Input id="pj-branch" className="rounded-lg type-code" value={branch} autoCapitalize="off" spellCheck={false} onChange={(e) => { setBranch(e.target.value); setError("") }} placeholder={t("pages:projects.cloneDefault")} /></Field>
          </>}
          <Field className="gap-1.5"><FieldLabel htmlFor="pj-path">{t(existing ? "pages:projects.dir" : "pages:projects.setupParent")}</FieldLabel>
            <Input id="pj-path" autoFocus className="rounded-lg type-code" value={path} onChange={(e) => pick(e.target.value)} placeholder={existing ? "~/code/my-project" : "~/code"} data-project-path />
            <DirBrowser path={path} node="hub" onPick={pick} onSeen={setSeenGit} />
            <FieldDescription className="type-footnote">{t(existing ? "pages:projects.browseHere" : "pages:projects.setupParentHint")}</FieldDescription>
          </Field>
          {!existing && <>
            <Field className="gap-1.5"><FieldLabel htmlFor="pj-directory">{t("pages:projects.setupDirName")}</FieldLabel>
              <Input id="pj-directory" className="rounded-lg type-code" value={directory} onChange={(e) => { setDirectory(e.target.value); setError(""); if (!named) setN(e.target.value.trim().slice(0, 40)) }} /></Field>
            <Field className="gap-1.5">
              <div className="flex items-center justify-between gap-2">
                <FieldLabel>{t("pages:projects.setupDestination")}</FieldLabel>
                <Button type="button" size="xs" variant="ghost" disabled={!parent || !dir} onClick={() => { recheck(i => i + 1); setError("") }}>{t("pages:projects.setupRecheck")}</Button>
              </div>
              <div className="min-w-0 rounded-lg border border-border px-3 py-2 type-footnote" aria-live="polite">
                {preview
                  ? <span className="type-code break-all" data-project-preview>{preview}</span>
                  : target && "error" in target
                    ? <span className="text-destructive" data-project-preview-error>{target.error}</span>
                    : <span className="text-muted-foreground">{t(parent && dir ? "pages:projects.setupChecking" : "pages:projects.setupNeedPath")}</span>}
              </div>
            </Field>
          </>}
          <Field className="gap-1.5"><FieldLabel htmlFor="pj-n">{t("pages:projects.name")}</FieldLabel><Input id="pj-n" className="rounded-lg" value={n} maxLength={40} onChange={(e) => { setN(e.target.value); setNamed(true) }} placeholder={t("pages:projects.projectNamePh")} /></Field>
          <Field className="gap-1.5"><div className="flex items-center justify-between"><FieldLabel htmlFor="pj-d">{t("pages:projects.desc")}</FieldLabel>
            {existing && <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={!parent || drafting} onClick={() => void draft()} data-describe>{drafting ? <Spinner className="size-3" /> : <SparklesIcon />}{t("pages:projects.draft")}</Button>}</div>
            <Textarea id="pj-d" className="min-h-20 rounded-lg" value={d} onChange={(e) => setD(e.target.value)} placeholder={t("pages:projects.projectDescPh")} /></Field>
          <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("pages:projects.defaultBot")}</FieldLabel>
            <Select align="end" value={bot} onChange={setBot} options={[{ v: "none", l: t("pages:projects.bySteve") }, ...Object.keys(st.bots).filter((k) => st.bots[k].on).map((k) => ({ v: k, l: <BotName k={k} /> }))]} /></Field>
          {git && <Field orientation="horizontal" className="justify-between"><div><FieldLabel>{t("pages:projects.worktree")}</FieldLabel>{existing && <FieldDescription className="type-footnote">{t("pages:projects.gitDetected")}</FieldDescription>}</div><Switch checked={wt} onCheckedChange={setWt} /></Field>}
        </FieldGroup>
        {error && <p className="rounded-lg border border-border p-3 type-callout text-destructive" role="alert" data-project-setup-error>{error}</p>}
        {submitting && <div role="status" className="flex items-center gap-2 type-callout text-muted-foreground" data-project-progress>
          <Spinner className="size-4" />
          {clone ? t(`pages:projects.cloneProgress.${progress?.phase ?? "connecting"}`) : t("pages:projects.setupSaving")}
          {progress?.percent != null && <span className="tabular-nums">{progress.percent}%</span>}
        </div>}
        <DialogFooter className="gap-2"><Button variant="outline" disabled={submitting} onClick={closeSheet}>{t("common:action.cancel")}</Button>
          <Button disabled={!ready || submitting || drafting} data-testid="project-go" onClick={() => void submit()}>{t(clone ? "pages:projects.cloneCreate" : "pages:projects.create")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
