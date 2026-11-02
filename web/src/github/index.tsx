/* GitHub 页（github.md §10）：接入（GitHub App 推荐，个人令牌备选）、关联的仓库、建议关联、添加仓库。
 * 只有 Rust Hub 有这些接口；拿不到时显示“还没有接入”的占位，不显示示例数据。 */
import { useHubEvents } from "@/store"
import { useCallback, useEffect, useLayoutEffect, useState } from "react"
import { ChevronRightIcon, KeyRoundIcon, PlusIcon, RefreshCwIcon, SparklesIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { st, toast } from "@/store"
import { Unauthenticated } from "@/api"
import { go, Page, Group, Row, SectionLabel, Meta, Empty, StatusDot } from "@/ui"
import { Select } from "@/views"
import { Unwired } from "@/unwired"
import { gh, submitManifest, type GhOverview, type GhRepo, type GhAvailable } from "@/api/github"
import { NotWired } from "@/api"
import { RepoView } from "@/github/repo"

/* ── shared ── */
export type Load<T> = { s: "loading" } | { s: "absent" } | { s: "error"; msg: string } | { s: "ok"; v: T }
export const errMsg = (e: unknown) => (e instanceof Error ? e.message : String(e))
/** GET /github, re-read on the `gh` / `connection` stream notices and when the tab comes back into view */
export function useOverview() {
  const [ov, setOv] = useState<Load<GhOverview>>({ s: "loading" })
  const load = useCallback(() => gh.overview().then((v) => setOv({ s: "ok", v })).catch((e) => {
    if (e instanceof Unauthenticated) { location.reload(); return }
    setOv(e instanceof NotWired ? { s: "absent" } : { s: "error", msg: errMsg(e) })
  }), [])
  useEffect(() => {
    void load()
    const h = () => { if (document.visibilityState === "visible") void load() }
    document.addEventListener("visibilitychange", h); return () => document.removeEventListener("visibilitychange", h)
  }, [load])
  useHubEvents(["gh", "connection"], () => void load())
  return [ov, load] as const
}
export const INGEST: Record<string, string> = { app: "GitHub App 推送", repo_hook: "仓库 Webhook", poll: "轮询" }
export function Health({ r }: { r: GhRepo }) {
  const [s, l] = !r.enabled ? ["idle", r.problem ?? "已停用"] : r.healthy ? ["done", "正常"] : ["fail", r.problem ?? "连接有问题"]
  return <span className="inline-flex items-center gap-1.5" data-gh-health={s}><StatusDot s={s} />{INGEST[r.ingest] ?? r.ingest} · {l}</span>
}
export function Loading() { return <div className="flex justify-center py-16" role="status" aria-label="加载中"><Spinner className="size-5 text-muted-foreground" /></div> }
export function Failed({ msg, retry }: { msg: string; retry: () => void }) {
  return <div className="flex flex-col items-center gap-3 py-16 text-center" role="alert"><div className="type-headline">读不到 GitHub 的数据</div><div className="type-callout text-muted-foreground">{msg}</div><Button size="sm" variant="outline" onClick={retry}><RefreshCwIcon />重试</Button></div>
}
const ICO = "flex size-9 items-center justify-center rounded-lg bg-muted text-muted-foreground"
function GhMark({ className }: { className?: string }) {
  /* the GitHub mark (lucide dropped brand icons) */
  return <svg viewBox="0 0 16 16" aria-hidden className={className} fill="currentColor"><path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z" /></svg>
}
export const GhIcon = () => <span className={ICO}><GhMark className="size-[18px]" /></span>
const projectName = (id: string) => st.projects.find((p) => p.id === id)?.name ?? id

/* one-shot query of #/github?error=…|app=created|link=<project>: read, then the hash is cleaned (which remounts the page) */
let flash: { error?: string; app?: string; link?: string } | null = null
function takeQuery() {
  if (!location.hash.includes("?")) return // StrictMode runs the effect twice; the first run already took it
  const p = new URLSearchParams(location.hash.split("?")[1] ?? "")
  flash = { error: p.get("error") ?? undefined, app: p.get("app") ?? undefined, link: p.get("link") ?? undefined }
  go("#/github")
}

export function GitHubView({ repo }: { repo?: string }) {
  const q = !repo && location.hash.includes("?")
  useLayoutEffect(() => { if (q) takeQuery() }, [q])
  if (q) return null
  return repo ? <RepoView id={repo} /> : <IndexView />
}

/* ── #/github ── */
function IndexView() {
  const [ov, load] = useOverview()
  const [note] = useState(() => flash)
  useEffect(() => { flash = null }, [])
  const [adding, setAdding] = useState<{ project?: string; connection?: string; gh_id?: number } | null>(note?.link ? { project: note.link } : null)
  const [pat, setPat] = useState(false)
  if (ov.s === "absent") return <Unwired id="github" />
  const v = ov.s === "ok" ? ov.v : null
  const connected = !!v?.connections.length
  const appConn = v?.connections.some((c) => c.kind === "app")
  return (
    <Page title="GitHub" sub="仓库、PR 和检查。关联到项目后，Steve 在验收时问你要不要开 PR，检查失败时把日志交给事项去修" max={720}
      actions={connected ? <Button size="sm" onClick={() => setAdding({})} data-gh-add><PlusIcon />添加仓库</Button> : undefined}>
      <div className="flex flex-col gap-8">
        {note?.error && <div role="alert" className="rounded-xl border border-destructive/40 px-4 py-3 type-callout" data-gh-error><div className="type-headline text-destructive">没有完成</div><div className="mt-0.5 text-muted-foreground">{note.error}</div>{note.error.includes("保险箱") && <a href="#/vault" className="mt-1 inline-block text-blue hover:underline">去启用保险箱</a>}</div>}
        {note?.app === "created" && !appConn && <div role="status" className="rounded-xl border px-4 py-3 type-callout" data-gh-created><div className="type-headline">GitHub App 已创建</div><div className="mt-0.5 text-muted-foreground">下一步把它安装到要让 Steve 管的仓库上；装好后 GitHub 会带你回到这里。</div></div>}
        {ov.s === "loading" ? <Loading /> : ov.s === "error" ? <Failed msg={ov.msg} retry={load} /> : v && <>
          {!connected && !v.apps.length && <ConnectCard onPat={() => setPat(true)} />}
          {v.apps.length > 0 && <section><SectionLabel>GitHub App</SectionLabel>
            <Group>{v.apps.map((a) => (
              <Row key={a.slug} icon={<GhIcon />} title={<span className="font-medium">{a.slug}</span>} sub={<Meta><span>{a.owner || "个人账号"}</span><span>评论和 PR 以 {a.slug}[bot] 的身份出现</span></Meta>} data-gh-app={a.slug}>
                <Button size="sm" variant={appConn ? "ghost" : "default"} nativeButton={false} render={<a href={a.html_url.replace(/\/$/, "") + "/installations/new"} />} data-gh-install>{appConn ? "安装到更多仓库" : "安装到仓库"}</Button>
              </Row>))}</Group></section>}
          {connected && <section><SectionLabel right={!v.connections.some((c) => c.kind === "pat") ? <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setPat(true)} data-gh-pat-open><KeyRoundIcon />用个人令牌</Button> : undefined}>连接</SectionLabel>
            <Group>{v.connections.map((c) => (
              <Row key={c.id} icon={<GhIcon />} title={c.account || c.id} sub={<Meta><span>{c.kind === "app" ? "GitHub App · 本公司的机器人" : "个人令牌 · 我的账号"}</span><span className="inline-flex items-center gap-1.5"><StatusDot s={c.status === "ok" ? "done" : "fail"} />{c.status === "ok" ? "正常" : c.status}</span></Meta>} data-gh-conn={c.kind} />))}</Group></section>}
          {v.suggestions.length > 0 && <section><SectionLabel>建议关联</SectionLabel>
            <Group>{v.suggestions.map((s) => (
              <Row key={s.project_id} icon={<span className={ICO}><SparklesIcon className="size-[18px]" strokeWidth={1.5} /></span>} title={<span>把 <span className="font-mono text-[13px]">{s.full_name}</span> 关联到项目 {projectName(s.project_id)}？</span>} sub="项目目录的 origin 指向这个仓库" data-gh-suggest={s.full_name}>
                <Button size="sm" variant="outline" onClick={() => void gh.link({ connection_id: s.connection_id, gh_id: s.gh_id, project_id: s.project_id }).then(() => { toast("已关联 " + s.full_name); void load() }, (e) => toast(errMsg(e)))}>关联</Button>
              </Row>))}</Group></section>}
          {connected && <section><SectionLabel>仓库 · {v.repos.length}</SectionLabel>
            {v.repos.length ? <Group>{v.repos.map((r) => (
              <Row key={r.id} icon={<GhIcon />} onClick={() => go("#/github/" + r.id)} data-gh-repo={r.full_name}
                title={<span className="font-mono text-[13.5px]">{r.full_name}</span>}
                sub={<Meta><span>{r.project ? "项目 " + r.project.name : "没有关联项目"}</span><Health r={r} /><span>{r.open_prs} 个打开的 PR</span></Meta>}>
                <ChevronRightIcon className="size-4 text-tertiary" /></Row>))}</Group>
              : <Empty title="还没有关联仓库" sub="添加一个仓库，再把它关联到项目；Steve 就能跟踪这个项目的 PR 和检查。" />}</section>}
        </>}
      </div>
      {pat && <PatDialog onClose={() => setPat(false)} onDone={() => { setPat(false); void load() }} />}
      {adding && v && <AddRepoDialog ov={v} init={adding} onClose={() => setAdding(null)} onDone={(r) => { setAdding(null); void load(); go("#/github/" + r.id) }} />}
    </Page>
  )
}

function ConnectCard({ onPat }: { onPat: () => void }) {
  const [owner, setOwner] = useState(""), [busy, setBusy] = useState(false)
  const create = async () => {
    setBusy(true)
    try { submitManifest(await gh.manifest(owner.trim() || undefined)) } catch (e) { setBusy(false); toast(errMsg(e)) }
  }
  return (
    <section className="flex flex-col gap-4 rounded-xl border px-4 py-4" data-gh-connect>
      <div className="flex items-start gap-3"><GhIcon />
        <div className="min-w-0"><div className="type-headline">接入 GitHub</div>
          <div className="mt-0.5 type-callout text-muted-foreground">推荐建一个 GitHub App：装到哪些仓库就收到哪些仓库的事件，令牌一小时就过期，评论和 PR 以机器人身份出现，你可以审查并批准 Steve 开的 PR。</div></div>
      </div>
      <FieldGroup className="gap-3">
        <Field className="gap-1.5"><FieldLabel htmlFor="gh-owner">建在哪个组织下（可选）</FieldLabel>
          <Input id="gh-owner" className="max-w-xs rounded-lg font-mono text-[13px]" value={owner} onChange={(e) => setOwner(e.target.value)} placeholder="留空：建在你的个人账号下" autoComplete="off" spellCheck={false} />
          <FieldDescription className="type-footnote">点下面的按钮会跳到 GitHub 确认创建，完成后自动回来。</FieldDescription></Field>
      </FieldGroup>
      <div className="flex flex-wrap gap-2">
        <Button onClick={() => void create()} disabled={busy} data-gh-create-app>{busy ? <Spinner className="size-3.5" /> : <GhMark className="size-4" />}创建 GitHub App（推荐）</Button>
        <Button variant="outline" onClick={onPat} data-gh-pat-open><KeyRoundIcon />用个人令牌</Button>
      </div>
    </section>
  )
}

function PatDialog({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const [token, setToken] = useState(""), [busy, setBusy] = useState(false)
  const save = async () => {
    setBusy(true)
    try { await gh.connectPat(token.trim()); toast("已接入个人令牌"); onDone() } catch (e) { toast(errMsg(e)) } finally { setBusy(false) }
  }
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[480px]" data-gh-pat-dialog>
        <DialogHeader><DialogTitle className="type-headline">用个人令牌接入</DialogTitle>
          <DialogDescription className="type-callout">以你本人的身份操作，PR 是你自己开的。令牌存在系统保险箱里，不会给 Bot 看到。</DialogDescription></DialogHeader>
        <Field className="gap-1.5"><FieldLabel htmlFor="gh-token">令牌</FieldLabel>
          <Input id="gh-token" type="password" autoFocus className="rounded-lg font-mono text-[13px]" value={token} onChange={(e) => setToken(e.target.value)} placeholder="github_pat_…" autoComplete="off" spellCheck={false} onKeyDown={(e) => { if (e.key === "Enter" && token.trim() && !busy) void save() }} />
          <FieldDescription className="type-footnote">GitHub 设置 → Developer settings → Fine-grained tokens：选好仓库，给 Contents、Pull requests、Issues 读写，Checks、Actions 读；要接收事件再给 Webhooks 读写。没有 Webhooks 权限的仓库改为每 2 分钟轮询。</FieldDescription></Field>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={onClose}>取消</Button><Button disabled={!token.trim() || busy} onClick={() => void save()} data-gh-pat-go>{busy && <Spinner className="size-3.5" />}接入</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/* 添加仓库：选连接 → 连接能看到的仓库 → 可选的项目 */
function AddRepoDialog({ ov, init, onClose, onDone }: { ov: GhOverview; init: { project?: string; connection?: string; gh_id?: number }; onClose: () => void; onDone: (r: GhRepo) => void }) {
  const [conn, setConn] = useState(init.connection ?? ov.connections[0]?.id ?? "")
  const [list, setList] = useState<GhAvailable[] | null>(null), [err, setErr] = useState("")
  const [pick, setPick] = useState<number | null>(init.gh_id ?? null), [busy, setBusy] = useState(false)
  const taken = new Set(ov.repos.map((r) => r.project?.id).filter(Boolean))
  const projects = st.projects.filter((p) => !p.archived && (!taken.has(p.id) || p.id === init.project))
  const [project, setProject] = useState(init.project && projects.some((p) => p.id === init.project) ? init.project : "none")
  useEffect(() => {
    if (!conn) return
    setList(null); setErr("")
    gh.available(conn).then(setList, (e) => setErr(errMsg(e)))
  }, [conn])
  const go_ = async () => {
    if (pick == null) return
    setBusy(true)
    try { const r = await gh.link({ connection_id: conn, gh_id: pick, project_id: project === "none" ? null : project }); toast("已关联 " + r.full_name); onDone(r) } catch (e) { toast(errMsg(e)) } finally { setBusy(false) }
  }
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[520px]" data-gh-add-dialog>
        <DialogHeader><DialogTitle className="type-headline">添加仓库</DialogTitle>
          <DialogDescription className="type-callout">一个项目最多关联一个仓库。关联后 Steve 跟踪这个仓库的 PR、检查和 Issue。</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4">
          {ov.connections.length > 1 && <Field orientation="horizontal" className="justify-between"><FieldLabel>连接</FieldLabel>
            <Select align="end" testId="gh-conn" value={conn} onChange={(x) => { setConn(x); setPick(null) }} options={ov.connections.map((c) => ({ v: c.id, l: `${c.account} · ${c.kind === "app" ? "App" : "个人令牌"}` }))} /></Field>}
          <div className="flex flex-col gap-1.5"><div className="type-label text-muted-foreground" id="gh-avail-label">仓库</div>
            {err ? <div role="alert" className="type-callout text-destructive">{err}</div> : !list ? <Loading /> : !list.length ? <div className="py-4 type-callout text-muted-foreground">这个连接看不到任何仓库。App 要先安装到仓库上；个人令牌要在 GitHub 上选好仓库。</div> :
              <div role="radiogroup" aria-labelledby="gh-avail-label" className="flex max-h-64 flex-col overflow-y-auto rounded-lg border">
                {list.map((a) => (
                  <label key={a.gh_id} className={"flex cursor-pointer items-center gap-3 border-b px-3 py-2.5 last:border-b-0 has-[:disabled]:cursor-default has-[:disabled]:opacity-60 " + (pick === a.gh_id ? "bg-accent" : "hover:bg-accent/60")} data-gh-avail={a.full_name}>
                    <input type="radio" name="gh-repo" className="accent-[var(--blue)]" checked={pick === a.gh_id} disabled={a.linked} onChange={() => setPick(a.gh_id)} />
                    <span className="min-w-0 flex-1 truncate font-mono text-[13px]">{a.full_name}</span>
                    <span className="type-footnote text-muted-foreground">{a.linked ? "已关联" : a.private ? "私有" : "公开"}</span>
                  </label>))}
              </div>}
          </div>
          <Field orientation="horizontal" className="justify-between"><div><FieldLabel>项目</FieldLabel><FieldDescription className="type-footnote">也可以以后在仓库页里改</FieldDescription></div>
            <Select align="end" testId="gh-project" value={project} onChange={setProject} options={[{ v: "none", l: "先不关联" }, ...projects.map((p) => ({ v: p.id, l: p.name }))]} /></Field>
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={onClose}>取消</Button><Button disabled={pick == null || busy} onClick={() => void go_()} data-gh-add-go>{busy && <Spinner className="size-3.5" />}关联</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/* ── 项目页的 “GitHub” 一行 ── */
export function GitHubLine({ project }: { project: string }) {
  const [ov, setOv] = useState<GhOverview | null>(null)
  useEffect(() => { gh.overview().then(setOv, () => setOv(null)) }, [project])
  if (!ov) return null // no GitHub module on this Hub (or not readable): the project page stays as it was
  const r = ov.repos.find((x) => x.project?.id === project), s = ov.suggestions.find((x) => x.project_id === project)
  return (
    <section data-gh-line><SectionLabel>GitHub</SectionLabel>
      <Group>{r
        ? <Row icon={<GhIcon />} onClick={() => go("#/github/" + r.id)} title={<span className="font-mono text-[13.5px]">{r.full_name}</span>} sub={<Meta><Health r={r} /><span>{r.open_prs} 个打开的 PR</span></Meta>}><ChevronRightIcon className="size-4 text-tertiary" /></Row>
        : s ? <Row icon={<GhIcon />} title={<span>关联到 <span className="font-mono text-[13px]">{s.full_name}</span>？</span>} sub="项目目录的 origin 指向这个仓库">
            <Button size="sm" variant="outline" onClick={() => void gh.link({ connection_id: s.connection_id, gh_id: s.gh_id, project_id: project }).then((x) => { toast("已关联 " + x.full_name); setOv({ ...ov, repos: [...ov.repos, x], suggestions: ov.suggestions.filter((y) => y !== s) }) }, (e) => toast(errMsg(e)))} data-gh-line-suggest>关联</Button></Row>
        : <Row icon={<GhIcon />} title="没有关联仓库" sub={ov.connections.length ? "关联后 Steve 跟踪这个项目的 PR 和检查" : "先接入 GitHub"}>
            <Button size="sm" variant="outline" onClick={() => go(ov.connections.length ? "#/github?link=" + encodeURIComponent(project) : "#/github")} data-gh-line-link>{ov.connections.length ? "关联仓库" : "接入 GitHub"}</Button></Row>}
      </Group>
    </section>
  )
}

/* “我的”列表里的入口（GitHub 不占主导航） */
export function GitHubMoreRow() {
  return <Row icon={<GhIcon />} title="GitHub" sub="仓库、PR 和检查" onClick={() => go("#/github")} data-more-github><ChevronRightIcon className="size-4 text-tertiary" /></Row>
}
