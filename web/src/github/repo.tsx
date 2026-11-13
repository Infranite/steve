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
import { Select } from "@/view-shared"
import { Unwired } from "@/unwired"
import { gh, type GhRepo, type GhPr, type GhIssue, type GhDelivery, type GhSettings } from "@/api/github"
import { NotWired } from "@/api"
import { useOverview, Health, Loading, Failed, errMsg, type Load } from "@/github/index"
import { t, useT } from "@/i18n"
import { Trans } from "react-i18next"

/* backend states → [status dot, label]; labels are looked up at render time so a language switch applies */
const checks = (k: string): [string, string] =>
  k === "success" ? ["done", t("github:checks.success")] : k === "failure" ? ["fail", t("github:checks.failure")] : k === "pending" ? ["run", t("github:checks.pending")] : ["idle", t("github:checks.none")]
const review = (k: string): string | undefined =>
  k === "requested" ? t("github:review.requested") : k === "approved" ? t("github:review.approved") : k === "changes_requested" ? t("github:review.changesRequested") : k === "commented" ? t("github:review.commented") : undefined
const delivery = (k: string): [string, string] =>
  k === "received" ? ["run", t("github:delivery.received")] : k === "ok" ? ["done", t("github:delivery.ok")] : k === "failed" ? ["fail", t("github:delivery.failed")] : ["idle", t("github:delivery.ignored")]

export function RepoView({ id }: { id: string }) {
  const t = useT()
  const [ov, load] = useOverview()
  if (ov.s === "absent") return <Unwired id="github" />
  const r = ov.s === "ok" ? ov.v.repos.find((x) => x.id === id) : undefined
  if (ov.s === "ok" && !r) return <Page title={t("github:repo.title")} back={["#/github", "GitHub"]}><Empty title={t("github:repo.notFound")} sub={t("github:repo.notFoundSub")} /></Page>
  return r ? <Repo key={r.id + ":" + r.revision} r={r} reload={load} /> : (
    <Page title={t("github:repo.title")} back={["#/github", "GitHub"]}>{ov.s === "error" ? <Failed msg={ov.msg} retry={load} /> : <Loading />}</Page>
  )
}

function Repo({ r: r0, reload }: { r: GhRepo; reload: () => void }) {
  const t = useT()
  const [r, setR] = useState(r0), [tab, setTab] = useState<"prs" | "issues" | "deliveries">("prs"), [syncing, setSyncing] = useState(false), [unlink, setUnlink] = useState(false)
  const save = async (p: { project_id?: string | null; settings?: Partial<GhSettings>; enabled?: boolean }) => {
    try { setR(await gh.patch(r.id, p)); toast(t("common:state.saved")) } catch (e) { toast(errMsg(e)) }
  }
  const set = <K extends keyof GhSettings>(k: K) => (v: GhSettings[K]) => void save({ settings: { [k]: v } as Partial<GhSettings> })
  const sync = async () => { setSyncing(true); try { await gh.sync(r.id); toast(t("github:repo.syncStarted")); setTimeout(reload, 2500) } catch (e) { toast(errMsg(e)) } finally { setSyncing(false) } }
  const s = r.settings
  return (
    <Page title={r.full_name} back={["#/github", "GitHub"]} max={760}
      sub={<Meta><span>{r.private ? t("github:repo.private") : t("github:repo.public")}</span><span><Trans t={t} i18nKey="github:repo.defaultBranch" values={{ branch: r.default_branch }} components={{ mono: <span className="font-mono" /> }} /></span><span>{t(r.connection.kind === "app" ? "github:repo.viaApp" : "github:repo.viaPat", { account: r.connection.account })}</span><Health r={r} />{r.last_delivery ? <span>{t("github:repo.lastDelivery", { time: tm(r.last_delivery) })}</span> : null}</Meta>}
      actions={<>
        <Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => void sync()} disabled={syncing} data-gh-sync>{syncing ? <Spinner className="size-3.5" /> : <RefreshCwIcon />}{t("github:repo.syncNow")}</Button>
        <Button size="sm" variant="ghost" className="text-muted-foreground" nativeButton={false} render={<a href={"https://github.com/" + r.full_name} target="_blank" rel="noreferrer" aria-label={t("github:repo.openOnGithub")} />}><ExternalLinkIcon /><span className="hidden sm:inline">{t("github:repo.openOnGithub")}</span></Button>
      </>}>
      <div className="flex flex-col gap-8">
        {r.problem && <div role="alert" className="rounded-xl bg-destructive/8 px-4 py-3 type-callout text-destructive">{r.problem}</div>}
        <section><SectionLabel>{t("github:repo.settings")}</SectionLabel>
          <Group>
            <Row title={t("github:repo.project")} sub={t("github:repo.projectSub")}>
              <Select align="end" testId="gh-repo-project" value={r.project?.id ?? "none"} onChange={(v) => void save({ project_id: v === "none" ? null : v })}
                options={[{ v: "none", l: t("github:repo.noLink") }, ...st.projects.filter((p) => !p.archived || p.id === r.project?.id).map((p) => ({ v: p.id, l: p.name }))]} /></Row>
            <Row title={t("github:repo.afterAccept")} sub={s.after_accept === "open_pr" ? <span className="flex flex-col gap-0.5" data-gh-open-pr-sub><span>{t("github:repo.openPrSub")}</span>{r.connection.kind === "pat" && <span>{t("github:repo.openPrPat", { account: r.connection.account })}</span>}</span> : t("github:repo.afterAcceptSub")}><Select align="end" testId="gh-after-accept" value={s.after_accept} onChange={(v) => set("after_accept")(v as GhSettings["after_accept"])} options={[{ v: "ask_pr", l: t("github:repo.askPr") }, ...(r.private || s.after_accept === "open_pr" ? [{ v: "open_pr", l: t("github:repo.openPr") }] : []), { v: "none", l: t("github:repo.noPr") }]} /></Row>
            <Row title={t("github:repo.finishOn")} sub={s.finish_on === "merged" ? t("github:repo.finishMergedSub") : t("github:repo.finishAcceptedSub")}><Select align="end" testId="gh-finish-on" value={s.finish_on} onChange={(v) => set("finish_on")(v as GhSettings["finish_on"])} options={[{ v: "accepted", l: t("github:repo.finishAccepted") }, { v: "merged", l: t("github:repo.finishMerged") }]} /></Row>
            <Row title={t("github:repo.mergeMethod")}><Segmented size="sm" value={s.merge_method} onChange={set("merge_method")} options={[["squash", "Squash"], ["merge", "Merge"], ["rebase", "Rebase"]] as const} /></Row>
            <Row title={t("github:repo.deleteBranch")} sub={t("github:repo.deleteBranchSub")}><Switch checked={s.delete_branch} onCheckedChange={set("delete_branch")} aria-label={t("github:repo.deleteBranch")} data-gh-delete-branch /></Row>
            <Row title={t("github:repo.ciFail")} sub={t("github:repo.ciFailSub")}><Select align="end" testId="gh-ci-fail" value={s.ci_fail} onChange={(v) => set("ci_fail")(v as GhSettings["ci_fail"])} options={[{ v: "supplement", l: t("github:repo.ciSupplement") }, { v: "signal", l: t("github:repo.ciSignal") }]} /></Row>
            <Row title={t("github:repo.draftPr")}><Switch checked={s.draft_pr} onCheckedChange={set("draft_pr")} aria-label={t("github:repo.draftPr")} data-gh-draft-pr /></Row>
            <Row title={t("github:repo.enabled")} sub={r.enabled ? t("github:repo.enabledSub") : t("github:repo.disabledSub")}><Switch checked={r.enabled} onCheckedChange={(v) => void save({ enabled: v })} aria-label={t("github:repo.enabled")} data-gh-enabled /></Row>
          </Group>
        </section>
        <section>
          <div className="mb-3"><Segmented value={tab} onChange={setTab} options={[["prs", t("github:repo.tabPrs", { n: r.open_prs })], ["issues", t("github:repo.tabIssues", { n: r.open_issues })], ["deliveries", t("github:repo.tabDeliveries")]] as const} /></div>
          <div key={tab} className="anim-in">{tab === "prs" ? <Prs repo={r.id} /> : tab === "issues" ? <Issues repo={r.id} /> : <Deliveries repo={r.id} />}</div>
        </section>
        <div><Button size="sm" variant="ghost" className="text-destructive" onClick={() => setUnlink(true)} data-gh-unlink><UnlinkIcon />{t("github:repo.unlink")}</Button></div>
      </div>
      <AlertDialog open={unlink} onOpenChange={setUnlink}>
        <AlertDialogContent>
          <AlertDialogHeader><AlertDialogTitle>{t("github:repo.unlinkTitle", { name: r.full_name })}</AlertDialogTitle>
            <AlertDialogDescription>{r.ingest === "repo_hook" ? t("github:repo.unlinkDescHook") : t("github:repo.unlinkDesc")}</AlertDialogDescription></AlertDialogHeader>
          <AlertDialogFooter><AlertDialogCancel>{t("common:action.cancel")}</AlertDialogCancel>
            <AlertDialogAction variant="destructive" onClick={() => void gh.unlink(r.id).then(() => { toast(t("github:repo.unlinked")); go("#/github") }, (e) => toast(errMsg(e)))} data-gh-unlink-go>{t("github:repo.unlink")}</AlertDialogAction></AlertDialogFooter>
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
  const t = useT()
  if (v.s === "loading") return <Loading />
  if (v.s === "absent") return <Empty title={t("github:list.absent")} />
  if (v.s === "error") return <Failed msg={v.msg} retry={retry} />
  return v.v.length ? <Group>{v.v.map(row)}</Group> : <Empty title={empty[0]} sub={empty[1]} />
}
const Ext = ({ href, label }: { href: string; label: string }) => (
  <Button size="icon-sm" variant="ghost" className="text-muted-foreground" nativeButton={false} render={<a href={href} target="_blank" rel="noreferrer" aria-label={label} />}><ExternalLinkIcon /></Button>
)
const ItemLink = ({ item }: { item: GhPr["item"] }) => item ? <a href={"#/items/" + item.id} className="inline-flex items-center gap-1 hover:text-foreground" title={item.title} data-gh-item={item.id}><StatusDot s={item.status} /><ItemNo id={item.id} /></a> : null

/** the open / all filter: the state is the API value, the chips show its label */
function StateChips({ value, onChange }: { value: "open" | "all"; onChange: (v: "open" | "all") => void }) {
  const t = useT()
  const open = t("github:list.open"), all = t("github:list.all")
  return <Chips className="mb-2" value={value === "all" ? all : open} onChange={(l) => onChange(l === all ? "all" : "open")} options={[open, all]} />
}

function Prs({ repo }: { repo: string }) {
  const t = useT()
  const [state, setState] = useState<"open" | "all">("open")
  const [v, retry] = useList(() => gh.prs(repo, state), repo + state)
  return (<>
    <StateChips value={state} onChange={setState} />
    <ListOf v={v} retry={retry} empty={[t("github:pr.empty"), t("github:pr.emptySub")]} row={(p: GhPr) => {
      const [dot, label] = checks(p.checks_state), rv = review(p.review_state), sep = t("github:shared.listSep")
      const failed = p.checks.filter((c) => c.state === "fail").map((c) => c.name)
      return (
        <Row key={p.id} data-gh-pr={p.number}
          title={<span className="flex min-w-0 items-baseline gap-2"><span className="shrink-0 font-mono tabular-nums text-muted-foreground">#{p.number}</span><span className="truncate">{p.title}</span></span>}
          sub={<Meta>
            {p.state !== "open" ? <span>{p.state === "merged" ? t("github:pr.merged") : t("github:pr.closed")}</span> : p.draft ? <span>{t("github:pr.draft")}</span> : null}
            <span className="inline-flex items-center gap-1.5" title={failed.length ? t("github:checks.failedTitle", { names: failed.join(sep) }) : undefined}><StatusDot s={dot} />{failed.length ? t("github:checks.withNames", { label, names: failed.slice(0, 3).join(sep) }) : label}</span>
            {rv && <span>{rv}</span>}
            {p.mergeable === "dirty" && <span className="text-destructive">{t("github:pr.conflict")}</span>}
            <span className="font-mono">{p.head_ref}</span>
            {p.opened_by === "steve" && <span>{t("github:pr.bySteve")}</span>}
            <ItemLink item={p.item} />
            <span className="tabular-nums">{tm(p.updated)}</span>
          </Meta>}>
          <Ext href={p.url} label={t("github:pr.open", { number: p.number })} />
        </Row>)
    }} />
  </>)
}

function Issues({ repo }: { repo: string }) {
  const t = useT()
  const [state, setState] = useState<"open" | "all">("open")
  const [v, retry] = useList(() => gh.issues(repo, state), repo + state)
  return (<>
    <StateChips value={state} onChange={setState} />
    <ListOf v={v} retry={retry} empty={[t("github:issue.empty"), t("github:issue.emptySub")]} row={(i: GhIssue) => (
      <Row key={i.id} data-gh-issue={i.number}
        title={<span className="flex min-w-0 items-baseline gap-2"><span className="shrink-0 font-mono tabular-nums text-muted-foreground">#{i.number}</span><span className="truncate">{i.title}</span></span>}
        sub={<Meta>{i.state !== "open" && <span>{t("github:issue.closed")}</span>}{i.labels.length > 0 && <span>{i.labels.join(t("github:shared.listSep"))}</span>}<ItemLink item={i.item} /></Meta>}>
        <Ext href={i.url} label={t("github:issue.open", { number: i.number })} />
      </Row>)} />
  </>)
}

function Deliveries({ repo }: { repo: string }) {
  const t = useT()
  const [v, retry] = useList(() => gh.deliveries(repo), repo)
  return (<>
    <div className="mb-2 flex items-center justify-between gap-2"><span className="type-footnote text-muted-foreground">{t("github:delivery.hint")}</span><Button size="xs" variant="ghost" className="text-muted-foreground" onClick={retry}><RefreshCwIcon />{t("common:action.refresh")}</Button></div>
    <ListOf v={v} retry={retry} empty={[t("github:delivery.empty"), t("github:delivery.emptySub")]} row={(d: GhDelivery) => {
      const [dot, label] = delivery(d.status)
      return <Row key={d.id} data-gh-delivery={d.status} title={<span className="font-mono">{d.event}{d.action ? "." + d.action : ""}</span>}
        sub={<Meta><span className="tabular-nums">{tm(d.received)}</span><span className="inline-flex items-center gap-1.5"><StatusDot s={dot} />{label}</span>{d.error && <span className="text-destructive">{d.error}</span>}{d.id.startsWith("poll:") && <span>{t("github:delivery.poll")}</span>}</Meta>} />
    }} />
  </>)
}
