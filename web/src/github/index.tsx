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
import { Select } from "@/view-shared"
import { connStatusOf } from "@/api/accounts"
import { cn } from "cn"
import { Unwired } from "@/unwired"
import { gh, submitManifest, type GhOverview, type GhRepo, type GhAvailable } from "@/api/github"
import { NotWired } from "@/api"
import { RepoView } from "@/github/repo"
import { Trans } from "react-i18next"
import { t, useT } from "@/i18n"

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
/** how a repository's events arrive (r.ingest, a backend value) as a label; unknown kinds show as they are */
export const ingestLabel = (k: string) =>
  k === "app" ? t("github:ingest.app") : k === "repo_hook" ? t("github:ingest.repoHook") : k === "poll" ? t("github:ingest.poll") : k
export function Health({ r }: { r: GhRepo }) {
  const t = useT()
  const [s, l] = !r.enabled ? ["idle", r.problem ?? t("github:shared.disabled")] : r.healthy ? ["done", t("github:shared.ok")] : ["fail", r.problem ?? t("github:shared.problem")]
  return <span className="inline-flex items-center gap-1.5" data-gh-health={s}><StatusDot s={s} />{t("github:shared.health", { ingest: ingestLabel(r.ingest), state: l })}</span>
}
export function Loading() { const t = useT(); return <div className="flex justify-center py-16" role="status" aria-label={t("github:shared.loading")}><Spinner className="size-5 text-muted-foreground" /></div> }
export function Failed({ msg, retry }: { msg: string; retry: () => void }) {
  const t = useT()
  return <div className="flex flex-col items-center gap-3 py-16 text-center" role="alert"><div className="type-headline">{t("github:shared.failed")}</div><div className="type-callout text-muted-foreground">{msg}</div><Button size="sm" variant="outline" onClick={retry}><RefreshCwIcon />{t("common:action.retry")}</Button></div>
}
const ICO = "flex size-9 items-center justify-center rounded-lg bg-muted text-muted-foreground"
function GhMark({ className }: { className?: string }) {
  /* the GitHub mark (lucide dropped brand icons) */
  return <svg viewBox="0 0 16 16" aria-hidden className={className} fill="currentColor"><path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z" /></svg>
}
export const GhIcon = () => <span className={ICO}><GhMark className="size-[18px]" /></span>
const projectName = (id: string) => st.projects.find((p) => p.id === id)?.name ?? id

/* one-shot query of #/github?error=…|app=created|link=<project>: read, then the hash is cleaned (which remounts the page) */
let flash: { error?: string; errorCode?: string; app?: string; link?: string } | null = null
function takeQuery() {
  if (!location.hash.includes("?")) return // StrictMode runs the effect twice; the first run already took it
  const p = new URLSearchParams(location.hash.split("?")[1] ?? "")
  flash = { error: p.get("error") ?? undefined, errorCode: p.get("error_code") ?? undefined, app: p.get("app") ?? undefined, link: p.get("link") ?? undefined }
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
  const t = useT()
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
    <Page title="GitHub" sub={t("github:index.sub")} max={720}
      actions={connected ? <Button size="sm" onClick={() => setAdding({})} data-gh-add><PlusIcon />{t("github:index.addRepo")}</Button> : undefined}>
      <div className="flex flex-col gap-8">
        {/* note.error is the Hub's message in the browser's language (a redirect from GitHub carries no UI language);
            a known note.errorCode is rendered from the web's own catalog instead */}
        {note?.error && <div role="alert" className="rounded-xl border border-destructive/40 px-4 py-3 type-callout" data-gh-error><div className="type-headline text-destructive">{t("github:index.notDone")}</div><div className="mt-0.5 text-muted-foreground">{note.errorCode === "vault_needed" ? t("github:index.vaultNeeded") : note.error}</div>{note.errorCode === "vault_needed" && <a href="#/vault" className="mt-1 inline-block text-blue hover:underline">{t("github:index.enableVault")}</a>}</div>}
        {note?.app === "created" && !appConn && <div role="status" className="rounded-xl border px-4 py-3 type-callout" data-gh-created><div className="type-headline">{t("github:index.appCreated")}</div><div className="mt-0.5 text-muted-foreground">{t("github:index.appCreatedSub")}</div></div>}
        {ov.s === "loading" ? <Loading /> : ov.s === "error" ? <Failed msg={ov.msg} retry={load} /> : v && <>
          {!connected && !v.apps.length && <ConnectCard onPat={() => setPat(true)} />}
          {v.apps.length > 0 && <section><SectionLabel>GitHub App</SectionLabel>
            <Group>{v.apps.map((a) => (
              <Row key={a.slug} icon={<GhIcon />} title={<span className="font-medium">{a.slug}</span>} sub={<Meta><span>{a.owner || t("github:index.personalAccount")}</span><span>{t("github:index.botIdentity", { slug: a.slug })}</span></Meta>} data-gh-app={a.slug}>
                <Button size="sm" variant={appConn ? "ghost" : "default"} className="max-sm:px-2" nativeButton={false} render={<a href={a.html_url.replace(/\/$/, "") + "/installations/new"} />} aria-label={appConn ? t("github:index.installMore") : t("github:index.install")} data-gh-install><PlusIcon className="sm:hidden" /><span className={cn(appConn && "max-sm:hidden")}>{appConn ? t("github:index.installMore") : t("github:index.install")}</span></Button>
              </Row>))}</Group></section>}
          {connected && <section><SectionLabel right={!v.connections.some((c) => c.kind === "pat") ? <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => setPat(true)} data-gh-pat-open><KeyRoundIcon />{t("github:shared.usePat")}</Button> : undefined}>{t("github:index.connections")}</SectionLabel>
            <Group>{v.connections.map((c) => (
              <Row key={c.id} icon={<GhIcon />} title={c.account || c.id} sub={<Meta><span>{c.kind === "app" ? t("github:index.connApp") : t("github:index.connPat")}</span><span className="inline-flex items-center gap-1.5"><StatusDot s={c.status === "ok" ? "done" : "fail"} />{c.status === "ok" ? t("github:shared.ok") : connStatusOf(c)[0]}</span></Meta>} data-gh-conn={c.kind} />))}</Group></section>}
          {v.suggestions.length > 0 && <section><SectionLabel>{t("github:index.suggestions")}</SectionLabel>
            <Group>{v.suggestions.map((s) => (
              <Row key={s.project_id} icon={<span className={ICO}><SparklesIcon className="size-[18px]" strokeWidth={1.5} /></span>} title={<span><Trans t={t} i18nKey="github:index.suggestTitle" values={{ repo: s.full_name, project: projectName(s.project_id) }} components={{ mono: <span className="font-mono text-[13px]" /> }} /></span>} sub={t("github:shared.linkHint")} data-gh-suggest={s.full_name}>
                <Button size="sm" variant="outline" onClick={() => void gh.link({ connection_id: s.connection_id, gh_id: s.gh_id, project_id: s.project_id }).then(() => { toast(t("github:shared.linked", { name: s.full_name })); void load() }, (e) => toast(errMsg(e)))}>{t("github:shared.link")}</Button>
              </Row>))}</Group></section>}
          {connected && <section><SectionLabel>{t("github:index.repos", { n: v.repos.length })}</SectionLabel>
            {v.repos.length ? <Group>{v.repos.map((r) => (
              <Row key={r.id} icon={<GhIcon />} onClick={() => go("#/github/" + r.id)} data-gh-repo={r.full_name}
                title={<span className="font-mono text-[13.5px]">{r.full_name}</span>}
                sub={<Meta><span>{r.project ? t("github:index.project", { name: r.project.name }) : t("github:index.noProject")}</span><Health r={r} /><span>{t("github:shared.openPrs", { count: r.open_prs })}</span></Meta>}>
                <ChevronRightIcon className="size-4 text-tertiary" /></Row>))}</Group>
              : <Empty title={t("github:index.noRepoYet")} sub={t("github:index.noRepoSub")} />}</section>}
        </>}
      </div>
      {pat && <PatDialog onClose={() => setPat(false)} onDone={() => { setPat(false); void load() }} />}
      {adding && v && <AddRepoDialog ov={v} init={adding} onClose={() => setAdding(null)} onDone={(r) => { setAdding(null); void load(); go("#/github/" + r.id) }} />}
    </Page>
  )
}

function ConnectCard({ onPat }: { onPat: () => void }) {
  const t = useT()
  const [owner, setOwner] = useState(""), [busy, setBusy] = useState(false)
  const create = async () => {
    setBusy(true)
    try { submitManifest(await gh.manifest(owner.trim() || undefined)) } catch (e) { setBusy(false); toast(errMsg(e)) }
  }
  return (
    <section className="flex flex-col gap-4 rounded-xl border px-4 py-4" data-gh-connect>
      <div className="flex items-start gap-3"><GhIcon />
        <div className="min-w-0"><div className="type-headline">{t("github:connect.title")}</div>
          <div className="mt-0.5 type-callout text-muted-foreground">{t("github:connect.intro")}</div></div>
      </div>
      <FieldGroup className="gap-3">
        <Field className="gap-1.5"><FieldLabel htmlFor="gh-owner">{t("github:connect.owner")}</FieldLabel>
          <Input id="gh-owner" className="max-w-xs rounded-lg font-mono text-[13px]" value={owner} onChange={(e) => setOwner(e.target.value)} placeholder={t("github:connect.ownerPlaceholder")} autoComplete="off" spellCheck={false} />
          <FieldDescription className="type-footnote">{t("github:connect.ownerHint")}</FieldDescription></Field>
      </FieldGroup>
      <div className="flex flex-wrap gap-2">
        <Button onClick={() => void create()} disabled={busy} data-gh-create-app>{busy ? <Spinner className="size-3.5" /> : <GhMark className="size-4" />}{t("github:connect.createApp")}</Button>
        <Button variant="outline" onClick={onPat} data-gh-pat-open><KeyRoundIcon />{t("github:shared.usePat")}</Button>
      </div>
    </section>
  )
}

function PatDialog({ onClose, onDone }: { onClose: () => void; onDone: () => void }) {
  const t = useT()
  const [token, setToken] = useState(""), [busy, setBusy] = useState(false)
  const save = async () => {
    setBusy(true)
    try { await gh.connectPat(token.trim()); toast(t("github:pat.done")); onDone() } catch (e) { toast(errMsg(e)) } finally { setBusy(false) }
  }
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[480px]" data-gh-pat-dialog>
        <DialogHeader><DialogTitle className="type-headline">{t("github:pat.title")}</DialogTitle>
          <DialogDescription className="type-callout">{t("github:pat.desc")}</DialogDescription></DialogHeader>
        <Field className="gap-1.5"><FieldLabel htmlFor="gh-token">{t("github:pat.token")}</FieldLabel>
          <Input id="gh-token" type="password" autoFocus className="rounded-lg font-mono text-[13px]" value={token} onChange={(e) => setToken(e.target.value)} placeholder="github_pat_…" autoComplete="off" spellCheck={false} onKeyDown={(e) => { if (e.key === "Enter" && token.trim() && !busy) void save() }} />
          <FieldDescription className="type-footnote">{t("github:pat.hint")}</FieldDescription></Field>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button><Button disabled={!token.trim() || busy} onClick={() => void save()} data-gh-pat-go>{busy && <Spinner className="size-3.5" />}{t("github:pat.connect")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/* 添加仓库：选连接 → 连接能看到的仓库 → 可选的项目 */
function AddRepoDialog({ ov, init, onClose, onDone }: { ov: GhOverview; init: { project?: string; connection?: string; gh_id?: number }; onClose: () => void; onDone: (r: GhRepo) => void }) {
  const t = useT()
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
    try { const r = await gh.link({ connection_id: conn, gh_id: pick, project_id: project === "none" ? null : project }); toast(t("github:shared.linked", { name: r.full_name })); onDone(r) } catch (e) { toast(errMsg(e)) } finally { setBusy(false) }
  }
  return (
    <Dialog open onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[520px]" data-gh-add-dialog>
        <DialogHeader><DialogTitle className="type-headline">{t("github:index.addRepo")}</DialogTitle>
          <DialogDescription className="type-callout">{t("github:add.desc")}</DialogDescription></DialogHeader>
        <FieldGroup className="gap-4">
          {ov.connections.length > 1 && <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("github:add.connection")}</FieldLabel>
            <Select align="end" testId="gh-conn" value={conn} onChange={(x) => { setConn(x); setPick(null) }} options={ov.connections.map((c) => ({ v: c.id, l: t("github:add.connOption", { account: c.account, kind: c.kind === "app" ? t("github:add.app") : t("github:shared.pat") }) }))} /></Field>}
          <div className="flex flex-col gap-1.5"><div className="type-label text-muted-foreground" id="gh-avail-label">{t("github:add.repo")}</div>
            {err ? <div role="alert" className="type-callout text-destructive">{err}</div> : !list ? <Loading /> : !list.length ? <div className="py-4 type-callout text-muted-foreground">{t("github:add.noneVisible")}</div> :
              <div role="radiogroup" aria-labelledby="gh-avail-label" className="flex max-h-64 flex-col overflow-y-auto rounded-lg border">
                {list.map((a) => (
                  <label key={a.gh_id} className={"flex cursor-pointer items-center gap-3 border-b px-3 py-2.5 last:border-b-0 has-[:disabled]:cursor-default has-[:disabled]:opacity-60 " + (pick === a.gh_id ? "bg-accent" : "hover:bg-accent/60")} data-gh-avail={a.full_name}>
                    <input type="radio" name="gh-repo" className="accent-[var(--blue)]" checked={pick === a.gh_id} disabled={a.linked} onChange={() => setPick(a.gh_id)} />
                    <span className="min-w-0 flex-1 truncate font-mono text-[13px]">{a.full_name}</span>
                    <span className="type-footnote text-muted-foreground">{a.linked ? t("github:add.alreadyLinked") : a.private ? t("github:repo.private") : t("github:repo.public")}</span>
                  </label>))}
              </div>}
          </div>
          <Field orientation="horizontal" className="justify-between"><div><FieldLabel>{t("github:add.project")}</FieldLabel><FieldDescription className="type-footnote">{t("github:add.projectHint")}</FieldDescription></div>
            <Select align="end" testId="gh-project" value={project} onChange={setProject} options={[{ v: "none", l: t("github:add.notYet") }, ...projects.map((p) => ({ v: p.id, l: p.name }))]} /></Field>
        </FieldGroup>
        <DialogFooter className="gap-2"><Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button><Button disabled={pick == null || busy} onClick={() => void go_()} data-gh-add-go>{busy && <Spinner className="size-3.5" />}{t("github:shared.link")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/* ── 项目页的 “GitHub” 一行 ── */
export function GitHubLine({ project }: { project: string }) {
  const t = useT()
  const [ov, setOv] = useState<GhOverview | null>(null)
  useEffect(() => { gh.overview().then(setOv, () => setOv(null)) }, [project])
  if (!ov) return null // no GitHub module on this Hub (or not readable): the project page stays as it was
  const r = ov.repos.find((x) => x.project?.id === project), s = ov.suggestions.find((x) => x.project_id === project)
  return (
    <section data-gh-line><SectionLabel>GitHub</SectionLabel>
      <Group>{r
        ? <Row icon={<GhIcon />} onClick={() => go("#/github/" + r.id)} title={<span className="font-mono text-[13.5px]">{r.full_name}</span>} sub={<Meta><Health r={r} /><span>{t("github:shared.openPrs", { count: r.open_prs })}</span></Meta>}><ChevronRightIcon className="size-4 text-tertiary" /></Row>
        : s ? <Row icon={<GhIcon />} title={<span><Trans t={t} i18nKey="github:line.suggestTitle" values={{ repo: s.full_name }} components={{ mono: <span className="font-mono text-[13px]" /> }} /></span>} sub={t("github:shared.linkHint")}>
            <Button size="sm" variant="outline" onClick={() => void gh.link({ connection_id: s.connection_id, gh_id: s.gh_id, project_id: project }).then((x) => { toast(t("github:shared.linked", { name: x.full_name })); setOv({ ...ov, repos: [...ov.repos, x], suggestions: ov.suggestions.filter((y) => y !== s) }) }, (e) => toast(errMsg(e)))} data-gh-line-suggest>{t("github:shared.link")}</Button></Row>
        : <Row icon={<GhIcon />} title={t("github:shared.noRepo")} sub={ov.connections.length ? t("github:line.linkedSub") : t("github:line.connectFirst")}>
            <Button size="sm" variant="outline" onClick={() => go(ov.connections.length ? "#/github?link=" + encodeURIComponent(project) : "#/github")} data-gh-line-link>{ov.connections.length ? t("github:line.linkRepo") : t("github:shared.connectGithub")}</Button></Row>}
      </Group>
    </section>
  )
}

/* “我的”列表里的入口（GitHub 不占主导航） */
export function GitHubMoreRow() {
  const t = useT()
  return <Row icon={<GhIcon />} title="GitHub" sub={t("github:index.moreSub")} onClick={() => go("#/github")} data-more-github><ChevronRightIcon className="size-4 text-tertiary" /></Row>
}
