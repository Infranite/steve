/* 工作空间与项目：工作空间是数据边界，切换后所有页面只显示这个空间的内容；一个工作空间管理多个项目，每个项目一个独立目录 */
import { api } from "@/api"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { Switch } from "@/components/ui/switch"
import { Textarea } from "@/components/ui/textarea"
import { AskSteve } from "@/config-ui"
import { GitHubLine } from "@/github/index"
import { LaneSection } from "@/lane"
import { RecentStats } from "@/stats/drill"
import { botName, newProject, newWorkspace, patchProject, st, toast } from "@/store"
import { BotName, closeSheet, Empty, go, Group, Meta, openSheet, Page, Row, SectionLabel } from "@/ui"
import { ItemRowExport as ItemRow, Select } from "@/view-shared"
import { YoloProjectSelect } from "@/yolo/badges"
import { cn } from "cn"
import { ArchiveIcon, ChevronRightIcon, FolderGit2Icon, FolderIcon, PlusIcon, SparklesIcon } from "lucide-react"
import { useEffect, useState } from "react"
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
  const ps = st.projects, running = (id: string) => st.items.filter((i) => i.project === id && ["run", "wait", "block"].includes(i.st) && !i.arch).length
  return (
    <Page title={t("pages:projects.title")} sub={t("pages:projects.sub", { space: st.workspace?.name })} max={720}
      actions={<Button size="sm" onClick={() => openSheet("newProject")} data-new-project><PlusIcon />{t("pages:projects.newProject")}</Button>}>
      {ps.length ? <Group>{ps.map((p) => { const I = projectIcon(p.kind); return (
        <Row key={p.id} icon={<span className="flex size-9 items-center justify-center rounded-lg bg-muted text-muted-foreground"><I className="size-[18px]" strokeWidth={1.5} /></span>} onClick={() => go("#/projects/" + p.id)} data-project-row={p.name}
          title={<span className="type-body font-medium">{p.name}</span>}
          sub={<Meta><span className="font-mono">{home(p.path)}</span><span>{p.kind === "git" ? (p.worktree ? t("pages:projects.gitWorktree") : "git") : t("pages:projects.plainDir")}</span>{p.default_bot && <span>{t("pages:projects.defaultBotIs", { bot: botName(p.default_bot) })}</span>}<span>{t("pages:projects.running", { count: running(p.id) })}</span>{p.release && <span>{t("pages:projects.releasable")}</span>}</Meta>}>
          <ChevronRightIcon className="size-4 text-tertiary" /></Row>) })}</Group>
        : <Empty title={t("pages:projects.empty")} sub={t("pages:projects.emptySub")} />}
      <div className="mt-3 px-1 type-footnote text-muted-foreground">{t("pages:projects.hint")}</div>
    </Page>
  )
}

export function ProjectView({ id }: { id: string }) {
  const t = useT()
  const p = st.projects.find((x) => x.id === id)
  const [n, setN] = useState(p?.name ?? ""), [d, setD] = useState(p?.description ?? "")
  useEffect(() => { setN(p?.name ?? ""); setD(p?.description ?? "") }, [p?.updated])
  if (!p) return <Page title={t("pages:projects.title")} back={["#/projects", t("pages:projects.title")]}><Empty title={t("pages:projects.notFound")} sub={t("pages:projects.notFoundSub")} /></Page>
  const mine = st.items.filter((i) => i.project === p.id && !["done", "drop"].includes(i.st) && !i.arch), done = st.items.filter((i) => i.project === p.id && ["done", "drop"].includes(i.st) && !i.arch).slice(0, 5)
  const dirty = n.trim() !== p.name || d.trim() !== p.description
  return (
    <Page title={p.name} back={["#/projects", t("pages:projects.title")]} max={720} sub={<Meta><span className="font-mono">{home(p.path)}</span><span>{p.kind === "git" ? t("pages:projects.gitRepo") : t("pages:projects.plainDir")}</span><span>{t("pages:projects.onDevice", { device: st.devices.find((x) => x.id === p.device_id)?.name ?? "Hub" })}</span><RecentStats project={p.id} /></Meta>}
      actions={<Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => { patchProject(p.id, { archived: !p.archived }, p.archived ? t("pages:projects.restored") : t("pages:projects.archived")); if (!p.archived) go("#/projects") }}><ArchiveIcon />{p.archived ? t("pages:projects.restore") : t("pages:projects.archive")}</Button>}>
      <div className="flex flex-col gap-8">
        <AskSteve key={p.id} ctx={{ kind: "project", id: p.id }} placeholder={t("pages:projects.askPh")} />
        <section><SectionLabel>{t("pages:projects.active", { count: mine.length })}</SectionLabel>{mine.length ? <Group>{mine.map((i) => <ItemRow key={i.id} i={i} proj={false} />)}</Group> : <Empty title={t("pages:projects.noActive")} />}</section>
        <GitHubLine project={p.id} />
        {p.kind === "git" && <LaneSection project={p.id} />}
        {done.length > 0 && <section><SectionLabel>{t("pages:projects.recentDone")}</SectionLabel><Group>{done.map((i) => <ItemRow key={i.id} i={i} proj={false} />)}</Group></section>}
        <section className="flex flex-col gap-4"><SectionLabel className="mb-0">{t("pages:projects.settings")}</SectionLabel>
          <FieldGroup className="gap-4">
            <Field className="gap-1.5"><FieldLabel className="type-label text-muted-foreground">{t("pages:projects.name")}</FieldLabel><Input className="rounded-lg" value={n} onChange={(e) => setN(e.target.value)} /></Field>
            <Field className="gap-1.5"><FieldLabel className="type-label text-muted-foreground">{t("pages:projects.desc")}</FieldLabel><Textarea className="min-h-20 rounded-lg" value={d} onChange={(e) => setD(e.target.value)} />
              <FieldDescription className="type-footnote">{t("pages:projects.descHint")}</FieldDescription></Field>
            {dirty && <div className="flex gap-2"><Button size="sm" onClick={() => patchProject(p.id, { name: n.trim() || p.name, description: d.trim() })}>{t("common:action.save")}</Button><Button size="sm" variant="ghost" onClick={() => { setN(p.name); setD(p.description) }}>{t("common:action.cancel")}</Button></div>}
          </FieldGroup>
          <Group>
            <Row title={t("pages:projects.defaultBot")} sub={t("pages:projects.defaultBotSub")}><Select align="end" testId="proj-bot" value={p.default_bot ?? "none"} onChange={(v) => patchProject(p.id, { default_bot: v === "none" ? null : v })} options={[{ v: "none", l: t("pages:projects.bySteve") }, ...Object.keys(st.bots).filter((k) => st.bots[k].on).map((k) => ({ v: k, l: <BotName k={k} /> }))]} /></Row>
            {p.kind === "git" && <Row title={t("pages:projects.worktree")} sub={t("pages:projects.worktreeSub")}><Switch checked={p.worktree} onCheckedChange={(v) => patchProject(p.id, { worktree: v })} data-proj-worktree /></Row>}
            {st.yolo && <Row title={t("pages:projects.yolo")} sub={p.kind === "git" && p.worktree ? t("pages:projects.yoloSubWorktree") : t("pages:projects.yoloSubReal")} data-proj-yolo><YoloProjectSelect id={p.id} realDir={p.kind !== "git" || !p.worktree} /></Row>}
            <Row title={t("pages:projects.release")} sub={p.release ? <span>{t("pages:projects.releaseWith")}<span className="font-mono">{home(p.release.cmd)}</span></span> : t("pages:projects.releaseNone")} />
            {p.release && p.kind === "git" && p.worktree && <Row title={t("pages:projects.doneWhen")} data-proj-done
              sub={p.release.done_when ? t("pages:projects.doneRemote") : t("pages:projects.doneLocal")}>
              <Select align="end" testId="proj-done" value={p.release.done_when ?? "local"}
                onChange={(v) => { const r = p.release!; patchProject(p.id, { release: v === "local" ? { kind: r.kind, cmd: r.cmd } : { ...r, done_when: v as "merge" | "tag" } }) }}
                options={[{ v: "local", l: t("pages:projects.optLocal") }, { v: "merge", l: t("pages:projects.optMerge") }, { v: "tag", l: t("pages:projects.optTag") }]} /></Row>}
          </Group>
        </section>
      </div>
    </Page>
  )
}

export function NewProjectDialog() {
  const t = useT()
  const [path, setPath] = useState(""), [n, setN] = useState(""), [d, setD] = useState(""), [bot, setBot] = useState("none"), [git, setGit] = useState<boolean | null>(null), [wt, setWt] = useState(true)
  const [dirs, setDirs] = useState<string[]>([]), [busy, setBusy] = useState(false)
  useEffect(() => {
    const base = path.endsWith("/") ? path : path.includes("/") ? path.slice(0, path.lastIndexOf("/") + 1) : "~/"
    const t = setTimeout(() => { api.dirs(base).then((xs) => setDirs(xs.filter((x) => x.toLowerCase().startsWith(path.toLowerCase())).slice(0, 8))).catch(() => setDirs([])) }, 200)
    return () => clearTimeout(t)
  }, [path])
  const draft = async () => {
    if (!path.trim()) return; setBusy(true)
    try { const r = await api.describeProject(path.trim()); if (!n) setN(r.name); setD(r.description); setGit(r.git) } catch (e) { toast((e as Error).message) } finally { setBusy(false) }
  }
  return (
    <Dialog open onOpenChange={(o) => !o && closeSheet()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[520px]" data-new-project-dialog>
        <DialogHeader><DialogTitle className="type-headline">{t("pages:projects.newProject")}</DialogTitle><DialogDescription className="type-callout">{t("pages:projects.newProjectDesc")}</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4">
          <Field className="gap-1.5"><FieldLabel htmlFor="pj-path">{t("pages:projects.dir")}</FieldLabel>
            <Input id="pj-path" autoFocus className="rounded-lg font-mono text-[13px]" value={path} onChange={(e) => { setPath(e.target.value); setGit(null) }} placeholder="~/code/my-project" data-project-path />
            {dirs.length > 0 && !dirs.includes(path) && <div className="flex flex-wrap gap-1">{dirs.map((x) => <button key={x} type="button" onClick={() => setPath(x)} className="rounded-md bg-muted px-2 py-0.5 font-mono text-[12px] text-muted-foreground hover:text-foreground">{x}</button>)}</div>}
          </Field>
          <Field className="gap-1.5"><FieldLabel htmlFor="pj-n">{t("pages:projects.name")}</FieldLabel><Input id="pj-n" className="rounded-lg" value={n} onChange={(e) => setN(e.target.value)} placeholder={t("pages:projects.projectNamePh")} /></Field>
          <Field className="gap-1.5"><div className="flex items-center justify-between"><FieldLabel htmlFor="pj-d">{t("pages:projects.desc")}</FieldLabel>
            <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={!path.trim() || busy} onClick={() => void draft()} data-describe>{busy ? <Spinner className="size-3" /> : <SparklesIcon />}{t("pages:projects.draft")}</Button></div>
            <Textarea id="pj-d" className="min-h-20 rounded-lg" value={d} onChange={(e) => setD(e.target.value)} placeholder={t("pages:projects.projectDescPh")} /></Field>
          <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("pages:projects.defaultBot")}</FieldLabel>
            <Select align="end" value={bot} onChange={setBot} options={[{ v: "none", l: t("pages:projects.bySteve") }, ...Object.keys(st.bots).filter((k) => st.bots[k].on).map((k) => ({ v: k, l: <BotName k={k} /> }))]} /></Field>
          {git && <Field orientation="horizontal" className="justify-between"><div><FieldLabel>{t("pages:projects.worktree")}</FieldLabel><FieldDescription className="type-footnote">{t("pages:projects.gitDetected")}</FieldDescription></div><Switch checked={wt} onCheckedChange={setWt} /></Field>}
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={closeSheet}>{t("common:action.cancel")}</Button>
          <Button disabled={!path.trim() || !n.trim()} className={cn()} data-testid="project-go" onClick={async () => { const id = await newProject({ name: n.trim(), path: path.trim(), description: d.trim(), worktree: wt, default_bot: bot === "none" ? null : bot }); if (id) { closeSheet(); go("#/projects/" + id) } }}>{t("pages:projects.create")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
