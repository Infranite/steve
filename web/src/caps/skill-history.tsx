import { useEffect, useMemo, useState } from "react"
import { capsApi, type Skill, type SkillDistribution, type SkillVersion } from "@/api/caps"
import { Badge } from "@/components/affect/badge-clean"
import { diffWords } from "@/components/affect/inline-diff-clean"
import { Markdown } from "@/components/markdown"
import { Button } from "@/components/ui/button"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { useT } from "@/i18n"
import { st } from "@/store"
import { Blank, Loading, Notice } from "@/ui"
import { errText, useList } from "./common"

export function SkillHistory({ skill, onPublished, onReload }: { skill: Skill; onPublished: (skill: Skill) => void; onReload: () => void }) {
  const t = useT()
  const [saved] = useList(() => capsApi.skills.versions(skill.id), [skill.id, skill.head_version, skill.publication])
  const [selected, select] = useState(skill.head_version)
  const [details, setDetails] = useState<{ version: SkillVersion; base: SkillVersion | null } | null>(null)
  const [detailError, setDetailError] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [confirm, setConfirm] = useState<"publish" | "rollback" | null>(null)
  const [busy, setBusy] = useState(false)
  const [compare, setCompare] = useState(true)
  useEffect(() => { select(skill.head_version) }, [skill.head_version])
  useEffect(() => {
    if (!selected) return
    const ctrl = new AbortController()
    setDetails(null); setDetailError(null)
    void Promise.all([
      capsApi.skills.version(skill.id, selected, ctrl.signal),
      skill.published_version ? capsApi.skills.version(skill.id, skill.published_version, ctrl.signal) : Promise.resolve(null),
    ]).then(([version, base]) => { if (!ctrl.signal.aborted) setDetails({ version, base }) },
      (e) => { if (!ctrl.signal.aborted) setDetailError(errText(e)) })
    return () => ctrl.abort()
  }, [skill.id, skill.published_version, selected])
  const v = details?.version
  const diff = useMemo(() => {
    if (!details?.base) return []
    const text = (b: SkillVersion) => `name: ${b.name}\ndescription: ${b.description}\n\n${b.content}`
    return diffWords(text(details.base), text(details.version), "line")
  }, [details])
  const publish = async (rollback: boolean) => {
    if (!v || busy) return
    setBusy(true); setError(null)
    try { onPublished(await capsApi.skills.publish(skill.id, v.version, skill.publication, rollback)) }
    catch (e) { setError(errText(e)) } finally { setBusy(false); setConfirm(null) }
  }
  return <div className="flex min-w-0 flex-col gap-4" data-skill-history>
    {saved.s === "ok" ? <label className="flex min-w-0 flex-col gap-1 type-label">
      {t("caps:skills.versions")}
      <select className="min-w-0 rounded-lg border bg-background p-2" value={selected} onChange={(e) => select(Number(e.target.value))} data-skill-version-select>
        {saved.v.map((s) => <option key={s.version} value={s.version}>
          v{s.version} · {s.name}{s.version === skill.published_version ? ` · ${t("caps:skills.published")}` : ""} · {new Date(s.saved_at).toLocaleString()}
        </option>)}
      </select>
    </label> : saved.s === "error" ? <Notice tone="destructive">{saved.e}</Notice> : saved.s === "unwired" ? <Blank>{t("caps:skills.unwired")}</Blank> : <Loading />}
    {detailError && <Notice tone="destructive">{detailError}</Notice>}
    {v && <>
      <p className="break-all type-footnote text-muted-foreground">v{v.version} · {new Date(v.saved_at).toLocaleString()}{v.saved_by ? ` · ${v.saved_by}` : ""}{v.note ? ` · ${v.note}` : ""}</p>
      <div className="flex flex-wrap gap-2">
        <Button size="xs" variant={compare ? "default" : "outline"} disabled={!details.base} onClick={() => setCompare(true)}>{t("caps:skills.compare")}</Button>
        <Button size="xs" variant={!compare || !details.base ? "default" : "outline"} onClick={() => setCompare(false)}>{t("caps:skills.preview")}</Button>
      </div>
      {compare && details.base ? <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-muted p-3 type-code" data-skill-diff>
        {diff.map((seg, i) => seg.type === "equal" ? <span key={i}>{seg.text}</span> : <span key={i}>
          {seg.remove && <del className="bg-destructive/15">{seg.remove}</del>}
          {seg.insert && <ins className="bg-[var(--aui-success-soft)]">{seg.insert}</ins>}
        </span>)}
      </pre> : <div className="max-h-64 overflow-auto break-words rounded-lg bg-muted p-3">
        <div className="font-medium">{v.name}</div><p className="type-footnote">{v.description}</p><Markdown text={v.content} />
      </div>}
      {v.version !== skill.published_version && <div className="flex flex-wrap justify-end gap-2">
        {v.first_published_at !== null
          ? <Button disabled={busy} variant="outline" onClick={() => setConfirm("rollback")} data-skill-rollback>{t("caps:skills.rollbackV", { version: v.version })}</Button>
          : <Button disabled={busy} onClick={() => setConfirm("publish")} data-skill-publish>{t("caps:skills.publishV", { version: v.version })}</Button>}
      </div>}
    </>}
    {error && <Notice tone="destructive">{error} <Button size="xs" variant="ghost" onClick={onReload}>{t("caps:skills.reloadVersion")}</Button></Notice>}
    <AlertDialog open={confirm !== null} onOpenChange={(o) => !o && !busy && setConfirm(null)}>
      <AlertDialogContent className="max-h-[85dvh] overflow-auto">
        <AlertDialogHeader>
          <AlertDialogTitle>{t(confirm === "rollback" ? "caps:skills.rollbackV" : "caps:skills.publishV", { version: v?.version ?? 0 })}</AlertDialogTitle>
          <AlertDialogDescription>{t("caps:skills.publishDesc")}</AlertDialogDescription>
        </AlertDialogHeader>
        {confirm && <SkillDistributionPanel id={skill.id} preview />}
        <AlertDialogFooter>
          <AlertDialogCancel disabled={busy}>{t("common:action.cancel")}</AlertDialogCancel>
          <AlertDialogAction disabled={busy} onClick={(e) => { e.preventDefault(); void publish(confirm === "rollback") }} data-skill-publish-confirm>
            {t(confirm === "rollback" ? "caps:skills.rollback" : "caps:skills.publish")}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </div>
}

export function SkillDistributionPanel({ id, preview = false }: { id: string; preview?: boolean }) {
  const t = useT()
  const [data, setData] = useState<SkillDistribution | null>(null)
  const [error, setError] = useState<string | null>(null)
  useEffect(() => {
    const ctrl = new AbortController()
    let timer: ReturnType<typeof setTimeout>
    const fetch = async () => {
      try {
        const result = await capsApi.skills.distribution(id, ctrl.signal)
        if (!ctrl.signal.aborted) { setData(result); setError(null) }
      } catch (e) { if (!ctrl.signal.aborted) { setData(null); setError(errText(e)) } }
      if (!ctrl.signal.aborted) timer = setTimeout(() => void fetch(), 4000)
    }
    void fetch()
    return () => { ctrl.abort(); clearTimeout(timer) }
  }, [id])
  const version = (n: number | null | undefined) => n ? `v${n}` : "—"
  if (error) return <Notice tone="destructive">{error}</Notice>
  if (!data) return <Loading />
  const nodes = preview ? data.nodes.filter((n) => n.enabled) : data.nodes
  return <div className="flex min-w-0 flex-col gap-3" data-skill-distribution>
    <p className="type-footnote text-muted-foreground">{t(preview ? "caps:skills.impactNote" : "caps:skills.distributionNote")}</p>
    {preview && nodes.length > 0 && <p className="type-footnote text-muted-foreground">{t("caps:skills.impactSummary", {
      bots: new Set(nodes.map((n) => n.bot)).size,
      eligible: new Set(nodes.filter((n) => n.eligible === true).map((n) => n.node)).size,
      offline: new Set(nodes.filter((n) => n.online === false).map((n) => n.node)).size,
    })}</p>}
    {nodes.length === 0 ? <Blank>{t("caps:skills.noTargets")}</Blank> : <ul className="divide-y rounded-lg border">
      {nodes.map((n) => <li key={`${n.bot}:${n.node ?? ""}`} className="flex min-w-0 flex-col gap-1 p-2" data-skill-node={n.node} data-skill-node-state={n.status}>
        <div className="flex flex-wrap items-center gap-2">
          <span className="break-all">{st.bots[n.bot]?.n ?? n.bot}{n.label ? ` · ${n.label}` : ""}</span>
          <Badge tone={n.status === "cached" ? "success" : n.status === "failed" ? "danger" : n.status === "pending" ? "info" : "neutral"}>
            {t(`caps:skills.status.${preview && n.online === false ? "offline" : n.status}`)}
          </Badge>
        </div>
        {!preview && <p className="break-all type-footnote text-muted-foreground">
          {t("caps:skills.distributionVersions", { desired: version(n.desired), confirmed: version(n.confirmed) })}
          {n.received_at ? ` · ${new Date(n.received_at).toLocaleString()}` : ""}{n.error ? ` · ${n.error}` : ""}
        </p>}
      </li>)}
    </ul>}
    {!preview && <>
      <h3 className="type-label">{t("caps:skills.sessionReceipts")}</h3>
      {data.sessions.length === 0 ? <Blank>{t("caps:skills.noSessions")}</Blank> : <ul className="divide-y rounded-lg border">
        {data.sessions.map((s) => <li key={s.session} className="flex flex-wrap items-center gap-2 break-all p-2 type-footnote" data-skill-session={s.session}>
          <span>{s.session} · {s.node} · {version(s.version)}</span>
          <Badge tone={s.status === "opened" ? "success" : s.status === "conflict" ? "warning" : "neutral"}>{t(`caps:skills.status.${s.status}`)}</Badge>
        </li>)}
      </ul>}
    </>}
  </div>
}
