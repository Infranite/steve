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

const home = (p: string) => p.replace(/^\/Users\/[^/]+/, "~")
export const projectIcon = (kind: string) => (kind === "git" ? FolderGit2Icon : FolderIcon)
export function NewWorkspaceDialog() {
  const [n, setN] = useState(""), [d, setD] = useState(""), [p, setP] = useState("")
  const prefix = p.trim().toUpperCase().replace(/[^A-Z]/g, "").slice(0, 6)
  return (
    <Dialog open onOpenChange={(o) => !o && closeSheet()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[480px]">
        <DialogHeader><DialogTitle className="type-headline">新建工作空间</DialogTitle><DialogDescription className="type-callout">工作空间是数据的边界：事项、项目、对话和 Bot 的记忆都按工作空间分开，互相看不到。比如“个人”“公司”“某个兴趣小组”。</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4">
          <Field><FieldLabel htmlFor="ws-n">名称</FieldLabel><Input id="ws-n" autoFocus className="rounded-lg" value={n} onChange={(e) => setN(e.target.value)} placeholder="例如：公司" /></Field>
          <Field><FieldLabel htmlFor="ws-d">说明</FieldLabel><Textarea id="ws-d" className="min-h-16 rounded-lg" value={d} onChange={(e) => setD(e.target.value)} placeholder="这个空间里做哪些事" /></Field>
          <Field><FieldLabel htmlFor="ws-p">事项编号前缀</FieldLabel><Input id="ws-p" className="w-28 rounded-lg font-mono uppercase" value={p} onChange={(e) => setP(e.target.value)} placeholder="ST" maxLength={6} />
            <FieldDescription className="type-footnote">新事项的编号，例如 {(prefix || "ST")}-1。和别的空间用同一个前缀时，编号会接着往下数，不会重复。</FieldDescription></Field>
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={closeSheet}>取消</Button><Button disabled={!n.trim()} onClick={() => { closeSheet(); void newWorkspace(n.trim(), d.trim(), prefix || undefined) }} data-testid="ws-go">新建并切换</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/* ── 项目 ── */
export function ProjectsView() {
  const ps = st.projects, running = (id: string) => st.items.filter((i) => i.project === id && ["run", "wait", "block"].includes(i.st) && !i.arch).length
  return (
    <Page title="项目" sub={`「${st.workspace?.name}」的项目。每个项目有自己的目录，事项选了项目就在这个目录里干活；不一定是 git 仓库`} max={720}
      actions={<Button size="sm" onClick={() => openSheet("newProject")} data-new-project><PlusIcon />新建项目</Button>}>
      {ps.length ? <Group>{ps.map((p) => { const I = projectIcon(p.kind); return (
        <Row key={p.id} icon={<span className="flex size-9 items-center justify-center rounded-lg bg-muted text-muted-foreground"><I className="size-[18px]" strokeWidth={1.5} /></span>} onClick={() => go("#/projects/" + p.id)} data-project-row={p.name}
          title={<span className="type-body font-medium">{p.name}</span>}
          sub={<Meta><span className="font-mono">{home(p.path)}</span><span>{p.kind === "git" ? (p.worktree ? "git · 每个事项一个分支" : "git") : "普通目录"}</span>{p.default_bot && <span>默认 {botName(p.default_bot)}</span>}<span>{running(p.id)} 件进行中</span>{p.release && <span>可发布</span>}</Meta>}>
          <ChevronRightIcon className="size-4 text-tertiary" /></Row>) })}</Group>
        : <Empty title="还没有项目" sub="项目是一个目录：代码仓库、文档目录都可以。没有项目的事项，Steve 会给它一个临时目录。" />}
      <div className="mt-3 px-1 type-footnote text-muted-foreground">建事项时，Steve 会按事项内容和每个项目的说明选项目；说明写得越具体，选得越准。也可以在事项里手动更换。</div>
    </Page>
  )
}

export function ProjectView({ id }: { id: string }) {
  const p = st.projects.find((x) => x.id === id)
  const [n, setN] = useState(p?.name ?? ""), [d, setD] = useState(p?.description ?? "")
  useEffect(() => { setN(p?.name ?? ""); setD(p?.description ?? "") }, [p?.updated])
  if (!p) return <Page title="项目" back={["#/projects", "项目"]}><Empty title="项目不存在" sub="可能在别的工作空间里，或者已经归档。" /></Page>
  const mine = st.items.filter((i) => i.project === p.id && !["done", "drop"].includes(i.st) && !i.arch), done = st.items.filter((i) => i.project === p.id && ["done", "drop"].includes(i.st) && !i.arch).slice(0, 5)
  const dirty = n.trim() !== p.name || d.trim() !== p.description
  return (
    <Page title={p.name} back={["#/projects", "项目"]} max={720} sub={<Meta><span className="font-mono">{home(p.path)}</span><span>{p.kind === "git" ? "git 仓库" : "普通目录"}</span><span>在 {st.devices.find((x) => x.id === p.device_id)?.name ?? "Hub"} 上</span><RecentStats project={p.id} /></Meta>}
      actions={<Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => { patchProject(p.id, { archived: !p.archived }, p.archived ? "已恢复" : "已归档；目录里的文件没有改动"); if (!p.archived) go("#/projects") }}><ArchiveIcon />{p.archived ? "恢复" : "归档"}</Button>}>
      <div className="flex flex-col gap-8">
        <AskSteve key={p.id} ctx={{ kind: "project", id: p.id }} placeholder="例如：默认交给开发 Bot；说明里加上“前端用 shadcn”" />
        <section><SectionLabel>进行中的事项 · {mine.length}</SectionLabel>{mine.length ? <Group>{mine.map((i) => <ItemRow key={i.id} i={i} proj={false} />)}</Group> : <Empty title="没有进行中的事项" />}</section>
        <GitHubLine project={p.id} />
        {p.kind === "git" && <LaneSection project={p.id} />}
        {done.length > 0 && <section><SectionLabel>最近完成</SectionLabel><Group>{done.map((i) => <ItemRow key={i.id} i={i} proj={false} />)}</Group></section>}
        <section className="flex flex-col gap-4"><SectionLabel className="mb-0">设置</SectionLabel>
          <FieldGroup className="gap-4">
            <Field className="gap-1.5"><FieldLabel className="type-label text-muted-foreground">名称</FieldLabel><Input className="rounded-lg" value={n} onChange={(e) => setN(e.target.value)} /></Field>
            <Field className="gap-1.5"><FieldLabel className="type-label text-muted-foreground">说明</FieldLabel><Textarea className="min-h-20 rounded-lg" value={d} onChange={(e) => setD(e.target.value)} />
              <FieldDescription className="type-footnote">Steve 按这段说明判断事项该不该放进这个项目</FieldDescription></Field>
            {dirty && <div className="flex gap-2"><Button size="sm" onClick={() => patchProject(p.id, { name: n.trim() || p.name, description: d.trim() })}>保存</Button><Button size="sm" variant="ghost" onClick={() => { setN(p.name); setD(p.description) }}>取消</Button></div>}
          </FieldGroup>
          <Group>
            <Row title="默认 Bot" sub="这个项目的事项默认交给谁；Steve 也会按内容换"><Select align="end" testId="proj-bot" value={p.default_bot ?? "none"} onChange={(v) => patchProject(p.id, { default_bot: v === "none" ? null : v })} options={[{ v: "none", l: "由 Steve 判断" }, ...Object.keys(st.bots).filter((k) => st.bots[k].on).map((k) => ({ v: k, l: <BotName k={k} /> }))]} /></Row>
            {p.kind === "git" && <Row title="每个事项单独开分支" sub={`在 worktree 里干活，分支名 steve/<编号>，不直接改当前分支`}><Switch checked={p.worktree} onCheckedChange={(v) => patchProject(p.id, { worktree: v })} data-proj-worktree /></Row>}
            {st.yolo && <Row title="YOLO 模式" sub={p.kind === "git" && p.worktree ? "这个项目的事项：跟随全局，或单独设成关、YOLO、完全放开" : "没有 worktree，Agent 直接改真实目录，默认关"} data-proj-yolo><YoloProjectSelect id={p.id} realDir={p.kind !== "git" || !p.worktree} /></Row>}
            <Row title="发布" sub={p.release ? <span>验收通过后可以申请发布：<span className="font-mono">{home(p.release.cmd)}</span></span> : "没有配置发布动作；事项验收后就结束"} />
            {p.release && p.kind === "git" && p.worktree && <Row title="完成标准" data-proj-done
              sub={p.release.done_when ? "核对远端后自动收尾；目标要求构建通过的事项，等构建成功才结束" : "验收后在本机发布上线才结束"}>
              <Select align="end" testId="proj-done" value={p.release.done_when ?? "local"}
                onChange={(v) => patchProject(p.id, { done_when: v === "local" ? null : v as "merge" | "tag" })}
                options={[{ v: "local", l: "本机发布上线" }, { v: "merge", l: "PR 合并" }, { v: "tag", l: "PR 合并并推送 tag" }]} /></Row>}
          </Group>
        </section>
      </div>
    </Page>
  )
}

export function NewProjectDialog() {
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
        <DialogHeader><DialogTitle className="type-headline">新建项目</DialogTitle><DialogDescription className="type-callout">选一个目录。是 git 仓库时，每个事项可以在自己的分支上干活。</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4">
          <Field className="gap-1.5"><FieldLabel htmlFor="pj-path">目录</FieldLabel>
            <Input id="pj-path" autoFocus className="rounded-lg font-mono text-[13px]" value={path} onChange={(e) => { setPath(e.target.value); setGit(null) }} placeholder="~/Agents/steve" data-project-path />
            {dirs.length > 0 && !dirs.includes(path) && <div className="flex flex-wrap gap-1">{dirs.map((x) => <button key={x} type="button" onClick={() => setPath(x)} className="rounded-md bg-muted px-2 py-0.5 font-mono text-[12px] text-muted-foreground hover:text-foreground">{x}</button>)}</div>}
          </Field>
          <Field className="gap-1.5"><FieldLabel htmlFor="pj-n">名称</FieldLabel><Input id="pj-n" className="rounded-lg" value={n} onChange={(e) => setN(e.target.value)} placeholder="例如：Steve" /></Field>
          <Field className="gap-1.5"><div className="flex items-center justify-between"><FieldLabel htmlFor="pj-d">说明</FieldLabel>
            <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={!path.trim() || busy} onClick={() => void draft()} data-describe>{busy ? <Spinner className="size-3" /> : <SparklesIcon />}让 Steve 看看目录再起草</Button></div>
            <Textarea id="pj-d" className="min-h-20 rounded-lg" value={d} onChange={(e) => setD(e.target.value)} placeholder="这个目录是什么、在这里做哪类事" /></Field>
          <Field orientation="horizontal" className="justify-between"><FieldLabel>默认 Bot</FieldLabel>
            <Select align="end" value={bot} onChange={setBot} options={[{ v: "none", l: "由 Steve 判断" }, ...Object.keys(st.bots).filter((k) => st.bots[k].on).map((k) => ({ v: k, l: <BotName k={k} /> }))]} /></Field>
          {git && <Field orientation="horizontal" className="justify-between"><div><FieldLabel>每个事项单独开分支</FieldLabel><FieldDescription className="type-footnote">检测到 git 仓库</FieldDescription></div><Switch checked={wt} onCheckedChange={setWt} /></Field>}
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={closeSheet}>取消</Button>
          <Button disabled={!path.trim() || !n.trim()} className={cn()} data-testid="project-go" onClick={async () => { const id = await newProject({ name: n.trim(), path: path.trim(), description: d.trim(), worktree: wt, default_bot: bot === "none" ? null : bot }); if (id) { closeSheet(); go("#/projects/" + id) } }}>新建</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
