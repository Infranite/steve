/* #/github/:repo —— 一个关联的仓库：设置（github.md §3.4）、PR、Issue、投递记录 */
import { useHubEvents } from "@/store"
import { useEffect, useState, type ReactNode } from "react"
import { ExternalLinkIcon, RefreshCwIcon, UnlinkIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/ui/switch"
import { Spinner } from "@/components/ui/spinner"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { st, tm, toast } from "@/store"
import { go, Page, Group, Row, SectionLabel, Meta, Empty, StatusDot, ItemNo, Segmented, Chips } from "@/ui"
import { Select } from "@/views"
import { Unwired } from "@/unwired"
import { gh, type GhRepo, type GhPr, type GhIssue, type GhDelivery, type GhSettings } from "@/api/github"
import { NotWired } from "@/api"
import { useOverview, Health, Loading, Failed, errMsg, type Load } from "@/github/index"

const CHECKS: Record<string, [string, string]> = { success: ["done", "检查通过"], failure: ["fail", "检查失败"], pending: ["run", "检查进行中"], none: ["idle", "没有检查"] }
const REVIEW: Record<string, string> = { requested: "等待评审", approved: "已批准", changes_requested: "要求修改", commented: "有评审意见" }
const DELIVERY: Record<string, [string, string]> = { ok: ["done", "已处理"], ignored: ["idle", "忽略"], failed: ["fail", "失败"] }

export function RepoView({ id }: { id: string }) {
  const [ov, load] = useOverview()
  if (ov.s === "absent") return <Unwired id="github" />
  const r = ov.s === "ok" ? ov.v.repos.find((x) => x.id === id) : undefined
  if (ov.s === "ok" && !r) return <Page title="仓库" back={["#/github", "GitHub"]}><Empty title="仓库不存在" sub="可能已经取消关联，或者在别的工作空间里。" /></Page>
  return r ? <Repo key={r.id + ":" + r.revision} r={r} reload={load} /> : (
    <Page title="仓库" back={["#/github", "GitHub"]}>{ov.s === "error" ? <Failed msg={ov.msg} retry={load} /> : <Loading />}</Page>
  )
}

function Repo({ r: r0, reload }: { r: GhRepo; reload: () => void }) {
  const [r, setR] = useState(r0), [tab, setTab] = useState<"prs" | "issues" | "deliveries">("prs"), [syncing, setSyncing] = useState(false), [unlink, setUnlink] = useState(false)
  const save = async (p: { project_id?: string | null; settings?: Partial<GhSettings>; enabled?: boolean }) => {
    try { setR(await gh.patch(r.id, p)); toast("已保存") } catch (e) { toast(errMsg(e)) }
  }
  const set = <K extends keyof GhSettings>(k: K) => (v: GhSettings[K]) => void save({ settings: { [k]: v } as Partial<GhSettings> })
  const sync = async () => { setSyncing(true); try { await gh.sync(r.id); toast("已开始同步，稍后刷新") ; setTimeout(reload, 2500) } catch (e) { toast(errMsg(e)) } finally { setSyncing(false) } }
  const s = r.settings
  return (
    <Page title={<span className="font-mono">{r.full_name}</span>} back={["#/github", "GitHub"]} max={760}
      sub={<Meta><span>{r.private ? "私有" : "公开"}</span><span>默认分支 <span className="font-mono">{r.default_branch}</span></span><span>{r.connection.kind === "app" ? "经 GitHub App" : "经个人令牌"} · {r.connection.account}</span><Health r={r} />{r.last_delivery ? <span>最近收到 {tm(r.last_delivery)}</span> : null}</Meta>}
      actions={<>
        <Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => void sync()} disabled={syncing} data-gh-sync>{syncing ? <Spinner className="size-3.5" /> : <RefreshCwIcon />}立即同步</Button>
        <Button size="sm" variant="ghost" className="text-muted-foreground" nativeButton={false} render={<a href={"https://github.com/" + r.full_name} target="_blank" rel="noreferrer" aria-label="在 GitHub 打开" />}><ExternalLinkIcon /><span className="hidden sm:inline">在 GitHub 打开</span></Button>
      </>}>
      <div className="flex flex-col gap-8">
        {r.problem && <div role="alert" className="rounded-xl border border-destructive/40 px-4 py-3 type-callout">{r.problem}</div>}
        <section><SectionLabel>设置</SectionLabel>
          <Group>
            <Row title="关联的项目" sub="一个项目最多关联一个仓库；这个项目的事项开 PR、看检查都用它">
              <Select align="end" testId="gh-repo-project" value={r.project?.id ?? "none"} onChange={(v) => void save({ project_id: v === "none" ? null : v })}
                options={[{ v: "none", l: "不关联" }, ...st.projects.filter((p) => !p.archived || p.id === r.project?.id).map((p) => ({ v: p.id, l: p.name }))]} /></Row>
            <Row title="验收通过后" sub="分支上有新提交、还没有 PR 时"><Select align="end" testId="gh-after-accept" value={s.after_accept} onChange={(v) => set("after_accept")(v as GhSettings["after_accept"])} options={[{ v: "ask_pr", l: "问我要不要开 PR" }, { v: "none", l: "不开 PR" }]} /></Row>
            <Row title="事项什么时候算完成" sub={s.finish_on === "merged" ? "检查全绿后问你要不要合并，合并后完成" : "开了 PR 就算完成"}><Select align="end" testId="gh-finish-on" value={s.finish_on} onChange={(v) => set("finish_on")(v as GhSettings["finish_on"])} options={[{ v: "accepted", l: "开了 PR 就完成" }, { v: "merged", l: "合并后才完成" }]} /></Row>
            <Row title="合并方式"><Segmented size="sm" value={s.merge_method} onChange={set("merge_method")} options={[["squash", "Squash"], ["merge", "Merge"], ["rebase", "Rebase"]] as const} /></Row>
            <Row title="合并后删除分支" sub="删除远端的事项分支"><Switch checked={s.delete_branch} onCheckedChange={set("delete_branch")} aria-label="合并后删除分支" data-gh-delete-branch /></Row>
            <Row title="检查失败时" sub="关联了事项的 PR"><Select align="end" testId="gh-ci-fail" value={s.ci_fail} onChange={(v) => set("ci_fail")(v as GhSettings["ci_fail"])} options={[{ v: "supplement", l: "交给事项修" }, { v: "signal", l: "只记录" }]} /></Row>
            <Row title="Steve 开的 PR 先开成草稿"><Switch checked={s.draft_pr} onCheckedChange={set("draft_pr")} aria-label="Steve 开的 PR 先开成草稿" data-gh-draft-pr /></Row>
            <Row title="接收这个仓库的事件" sub={r.enabled ? "PR、检查和 Issue 的变化会同步进来" : "已停用：不再同步，也不产生信号"}><Switch checked={r.enabled} onCheckedChange={(v) => void save({ enabled: v })} aria-label="接收这个仓库的事件" data-gh-enabled /></Row>
          </Group>
        </section>
        <section>
          <div className="mb-3"><Segmented value={tab} onChange={setTab} options={[["prs", `PR · ${r.open_prs}`], ["issues", `Issue · ${r.open_issues}`], ["deliveries", "投递记录"]] as const} /></div>
          {tab === "prs" ? <Prs repo={r.id} /> : tab === "issues" ? <Issues repo={r.id} /> : <Deliveries repo={r.id} />}
        </section>
        <div><Button size="sm" variant="ghost" className="text-destructive" onClick={() => setUnlink(true)} data-gh-unlink><UnlinkIcon />取消关联</Button></div>
      </div>
      <AlertDialog open={unlink} onOpenChange={setUnlink}>
        <AlertDialogContent>
          <AlertDialogHeader><AlertDialogTitle>取消关联 {r.full_name}？</AlertDialogTitle>
            <AlertDialogDescription>Steve 不再跟踪这个仓库，这里的 PR 和 Issue 记录会删除；事项上的产出和链接保留。{r.ingest === "repo_hook" ? "Steve 建的仓库 Webhook 也会删掉。" : ""}</AlertDialogDescription></AlertDialogHeader>
          <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction variant="destructive" onClick={() => void gh.unlink(r.id).then(() => { toast("已取消关联"); go("#/github") }, (e) => toast(errMsg(e)))} data-gh-unlink-go>取消关联</AlertDialogAction></AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </Page>
  )
}

/** a list tab: loads on mount and when `dep` changes */
function useList<T>(f: () => Promise<T[]>, dep: string) {
  const [v, setV] = useState<Load<T[]>>({ s: "loading" })
  const load = () => { setV({ s: "loading" }); f().then((x) => setV({ s: "ok", v: x }), (e) => setV(e instanceof NotWired ? { s: "absent" } : { s: "error", msg: errMsg(e) })) }
  useEffect(load, [dep])
  useHubEvents(["gh"], () => f().then((x) => setV({ s: "ok", v: x }), () => {}))
  return [v, load] as const
}
function ListOf<T>({ v, retry, empty, row }: { v: Load<T[]>; retry: () => void; empty: [string, string]; row: (x: T) => ReactNode }) {
  if (v.s === "loading") return <Loading />
  if (v.s === "absent") return <Empty title="这个 Hub 没有提供这项数据" />
  if (v.s === "error") return <Failed msg={v.msg} retry={retry} />
  return v.v.length ? <Group>{v.v.map(row)}</Group> : <Empty title={empty[0]} sub={empty[1]} />
}
const Ext = ({ href, label }: { href: string; label: string }) => (
  <Button size="icon-sm" variant="ghost" className="text-muted-foreground" nativeButton={false} render={<a href={href} target="_blank" rel="noreferrer" aria-label={label} />}><ExternalLinkIcon /></Button>
)
const ItemLink = ({ item }: { item: GhPr["item"] }) => item ? <a href={"#/items/" + item.id} className="inline-flex items-center gap-1 hover:text-foreground" title={item.title} data-gh-item={item.id}><StatusDot s={item.status} /><ItemNo id={item.id} /></a> : null

function Prs({ repo }: { repo: string }) {
  const [state, setState] = useState<"打开的" | "全部">("打开的")
  const [v, retry] = useList(() => gh.prs(repo, state === "全部" ? "all" : "open"), repo + state)
  return (<>
    <Chips className="mb-2" value={state} onChange={setState} options={["打开的", "全部"] as const} />
    <ListOf v={v} retry={retry} empty={["没有 PR", "Steve 开的 PR 和推到事项分支的 PR 会出现在这里"]} row={(p: GhPr) => {
      const [dot, label] = CHECKS[p.checks_state] ?? CHECKS.none
      const failed = p.checks.filter((c) => c.state === "fail").map((c) => c.name)
      return (
        <Row key={p.id} data-gh-pr={p.number}
          title={<span className="flex min-w-0 items-baseline gap-2"><span className="shrink-0 font-mono text-[12.5px] text-muted-foreground">#{p.number}</span><span className="truncate">{p.title}</span></span>}
          sub={<Meta>
            {p.state !== "open" ? <span>{p.state === "merged" ? "已合并" : "已关闭"}</span> : p.draft ? <span>草稿</span> : null}
            <span className="inline-flex items-center gap-1.5" title={failed.length ? "失败：" + failed.join("、") : undefined}><StatusDot s={dot} />{label}{failed.length ? "：" + failed.slice(0, 3).join("、") : ""}</span>
            {REVIEW[p.review_state] && <span>{REVIEW[p.review_state]}</span>}
            {p.mergeable === "dirty" && <span className="text-destructive">有冲突</span>}
            <span className="font-mono">{p.head_ref}</span>
            {p.opened_by === "steve" && <span>Steve 开的</span>}
            <ItemLink item={p.item} />
            <span>{tm(p.updated)}</span>
          </Meta>}>
          <Ext href={p.url} label={`在 GitHub 打开 PR #${p.number}`} />
        </Row>)
    }} />
  </>)
}

function Issues({ repo }: { repo: string }) {
  const [state, setState] = useState<"打开的" | "全部">("打开的")
  const [v, retry] = useList(() => gh.issues(repo, state === "全部" ? "all" : "open"), repo + state)
  return (<>
    <Chips className="mb-2" value={state} onChange={setState} options={["打开的", "全部"] as const} />
    <ListOf v={v} retry={retry} empty={["没有 Issue", "仓库里新开的 Issue 会同步到这里"]} row={(i: GhIssue) => (
      <Row key={i.id} data-gh-issue={i.number}
        title={<span className="flex min-w-0 items-baseline gap-2"><span className="shrink-0 font-mono text-[12.5px] text-muted-foreground">#{i.number}</span><span className="truncate">{i.title}</span></span>}
        sub={<Meta>{i.state !== "open" && <span>已关闭</span>}{i.labels.length > 0 && <span>{i.labels.join("、")}</span>}<ItemLink item={i.item} /></Meta>}>
        <Ext href={i.url} label={`在 GitHub 打开 Issue #${i.number}`} />
      </Row>)} />
  </>)
}

function Deliveries({ repo }: { repo: string }) {
  const [v, retry] = useList(() => gh.deliveries(repo), repo)
  return (<>
    <div className="mb-2 flex items-center justify-between gap-2"><span className="type-footnote text-muted-foreground">GitHub 发来的最近 50 次事件，调试 Webhook 用</span><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={retry}><RefreshCwIcon />刷新</Button></div>
    <ListOf v={v} retry={retry} empty={["还没有收到事件", r0Hint]} row={(d: GhDelivery) => {
      const [dot, label] = DELIVERY[d.status] ?? DELIVERY.ignored
      return <Row key={d.id} data-gh-delivery={d.status} title={<span className="font-mono text-[13px]">{d.event}{d.action ? "." + d.action : ""}</span>}
        sub={<Meta><span>{tm(d.received)}</span><span className="inline-flex items-center gap-1.5"><StatusDot s={dot} />{label}</span>{d.error && <span className="text-destructive">{d.error}</span>}{d.id.startsWith("poll:") && <span>轮询</span>}</Meta>} />
    }} />
  </>)
}
const r0Hint = "App 和仓库 Webhook 的事件会记在这里；轮询模式下看到的变化也会记一条"
