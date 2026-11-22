/* Bot Team configuration and per-Bot execution grants, scoped to the workspace they were loaded in. */
import { useEffect, useId, useState } from "react"
import { ArrowDownIcon, ArrowUpIcon, PlusIcon, RefreshCwIcon } from "lucide-react"
import { modReq, isNotWired, type ModuleScope, type RItemDetail } from "@/api"
import { hubBase } from "@/hub"
import { useT } from "@/i18n"
import { botName, st } from "@/store"
import { Badge } from "@/components/affect/badge-clean"
import { Checkbox } from "@/components/affect/checkbox-clean"
import { TextField } from "@/components/affect/text-field-clean"
import { Button } from "@/components/ui/button"
import { SetSelect } from "@/settings-controls"
import { Card, Section } from "@/ui"

/** The Team choice that asks for no Team, even where the workspace has a default one. */
export const NO_TEAM = "none"
export type TeamMember = { bot: string; nodes: string[] | null }
export type BotTeam = { id: string; name: string; members: TeamMember[] }
type TeamList = { default: string | null; teams: BotTeam[]; editable: boolean }
type Candidate = { id: string; name: string | null; state: "candidate" | "pending" | "unavailable"; reason: string | null; checked_at: number | null }
type NodeList = { editable: boolean; bindings: string[]; candidates: Candidate[]; unavailable_bindings: number }
export type ItemDispatch = {
  team: { id: string; name: string; source: "default" | "explicit" | "inherited" } | null
  placement: { bot: string | null; node: string | null; node_name?: string; session: string | null; reason: string; skipped?: string[] } | null
}
const msg = (e: unknown) => e instanceof Error ? e.message : String(e)
const nodesPath = (id: string) => `/bots/${encodeURIComponent(id)}/execution-nodes`
const teamsPath = "/bot-teams"
const activeScope = (): ModuleScope => ({ workspace: st.workspace?.id ?? "", hub: hubBase() })
const reasonLabel = (t: ReturnType<typeof useT>, reason: string) => t(`manage:dispatch.reason.${reason}`, { defaultValue: t("manage:dispatch.reason.bind_failed") })

function useDispatchLoad<T>(path: string, poll = 0) {
  const scope = activeScope()
  const key = JSON.stringify([scope, path])
  const [result, setResult] = useState<{ key: string; value?: T; error?: string }>()
  const [rev, setRev] = useState(0)
  useEffect(() => {
    const cancel = new AbortController()
    const load = () => modReq<T>("GET", path, undefined, cancel.signal, scope).then(
      value => { if (!cancel.signal.aborted) setResult({ key, value }) },
      e => { if (!cancel.signal.aborted) setResult({ key, error: isNotWired(e) ? undefined : msg(e) }) },
    )
    void load()
    const timer = poll ? window.setInterval(load, poll) : undefined
    return () => { cancel.abort(); window.clearInterval(timer) }
  // The key includes every part of scope and the resource.
  }, [key, rev, poll])
  return { value: result?.key === key ? result.value : undefined, error: result?.key === key ? result.error : undefined, scope, reload: () => setRev(v => v + 1) }
}
export const useBotTeams = () => useDispatchLoad<TeamList>(teamsPath)
const nodeLabel = (t: ReturnType<typeof useT>, n: Candidate) => n.name || (n.id === "local" ? t("manage:dispatch.local") : n.id)

export function ExecutionNodes({ bot }: { bot: string }) {
  const load = useDispatchLoad<NodeList>(nodesPath(bot), 12000)
  const t = useT()
  const [draft, setDraft] = useState<string[]>()
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState("")
  const [saved, setSaved] = useState(false)
  const { value } = load
  if (!value) return load.error ? <p role="alert">{load.error}</p> : null
  const ids = draft ?? value.bindings
  const list = value.candidates
  // Prefer the ordered selected list, then the unbound candidates in report order.
  const ordered = [...ids.flatMap(id => list.filter(n => n.id === id)), ...list.filter(n => !ids.includes(n.id))]
  const move = (id: string, by: number) => {
    const next = [...ids], at = next.indexOf(id), to = at + by
    if (at < 0 || to < 0 || to >= next.length) return
    next.splice(at, 1); next.splice(to, 0, id); setDraft(next); setSaved(false)
  }
  const change = (next: typeof draft) => { setDraft(next); setSaved(false) }
  const save = async () => {
    if (draft === undefined) return
    setBusy(true); setErr(""); setSaved(false)
    try {
      await modReq("PUT", nodesPath(bot), { nodes: draft }, undefined, load.scope)
      setDraft(undefined); load.reload(); setSaved(true)
    } catch (e) { setErr(msg(e)) } finally { setBusy(false) }
  }
  const refresh = async () => {
    setBusy(true); setErr("")
    try { await modReq("GET", "/system/models?fresh=1", undefined, undefined, load.scope); load.reload() }
    catch (e) { setErr(msg(e)) } finally { setBusy(false) }
  }
  return <Section label={t("manage:dispatch.executionNodes")} right={<Button size="xs" variant="ghost" disabled={busy} onClick={() => void refresh()} aria-label={t("manage:dispatch.refresh")}><RefreshCwIcon /></Button>}>
    <Card className="flex flex-col gap-3 p-4" data-execution-nodes>
      <p className="type-footnote text-muted-foreground">{t("manage:dispatch.probeHint")}</p>
      <p className="type-footnote text-muted-foreground">{t("manage:dispatch.nodesHint")}</p>
      {value.unavailable_bindings > 0 && <p className="type-footnote text-attention">{t("manage:dispatch.hiddenBindings")}</p>}
      <ul className="flex flex-col gap-2">
        {ordered.map(n => {
          const at = ids.indexOf(n.id), name = nodeLabel(t, n)
          return <li key={n.id} data-execution-node={n.id} className="flex flex-wrap items-center gap-2 border-t pt-2">
            <div className="min-w-0 flex-1 basis-40">
              <Checkbox checked={at >= 0} disabled={!value.editable || busy || at < 0 && n.state !== "candidate"}
                label={name} data-bind-node={n.id} onChange={e => change(e.currentTarget.checked ? [...ids, n.id] : ids.filter(id => id !== n.id))} />
            </div>
            <Badge tone={n.state === "candidate" ? "success" : n.state === "pending" ? "warning" : "neutral"}>{t(`manage:dispatch.${n.state}`)}</Badge>
            {at >= 0 && <Badge tone="info">{at === 0 ? t("manage:dispatch.preferred") : t("manage:dispatch.backup", { n: at })}</Badge>}
            {value.editable && at >= 0 && <div className="flex gap-1">
              <Button size="icon-xs" variant="ghost" disabled={busy || at === 0} onClick={() => move(n.id, -1)} aria-label={t("manage:dispatch.moveUp", { name })}><ArrowUpIcon /></Button>
              <Button size="icon-xs" variant="ghost" disabled={busy || at === ids.length - 1} onClick={() => move(n.id, 1)} aria-label={t("manage:dispatch.moveDown", { name })}><ArrowDownIcon /></Button>
            </div>}
            <p className="basis-full type-footnote text-muted-foreground">{[n.reason && reasonLabel(t, n.reason), n.checked_at && t("manage:dispatch.checked", { at: new Date(n.checked_at).toLocaleString() })].filter(Boolean).join(" · ")}</p>
          </li>
        })}
      </ul>
      {err && <p role="alert" className="type-footnote text-destructive">{err}</p>}
      {saved && <p role="status" className="type-footnote text-muted-foreground">{t("manage:dispatch.saved")}</p>}
      {value.editable && <div><Button size="sm" data-save-execution-nodes disabled={busy || draft === undefined} onClick={() => void save()}>{t("manage:dispatch.saveBindings")}</Button></div>}
    </Card>
  </Section>
}

function TeamNodeRange({ bot, nodes, update, disabled }: { bot: string; nodes: string[] | null; update: (ids: string[] | null) => void; disabled: boolean }) {
  const t = useT()
  const { value, error } = useDispatchLoad<NodeList>(nodesPath(bot))
  const bound = value?.bindings ?? []
  const ids = [...new Set([...bound, ...(nodes ?? [])])]
  return <div className="flex flex-col gap-2 pl-2">
    <Checkbox checked={nodes === null} disabled={disabled} label={t("manage:dispatch.allBound")}
      onChange={e => update(e.currentTarget.checked ? null : [])} />
    {nodes !== null && ids.map(id => {
      const n = value?.candidates.find(n => n.id === id)
      return <Checkbox key={id} checked={nodes.includes(id)} disabled={disabled || !bound.includes(id) && !nodes.includes(id)}
        label={n ? nodeLabel(t, n) : t("manage:dispatch.unavailable")}
        data-team-node={id} onChange={e => update(e.currentTarget.checked ? [...nodes, id] : nodes.filter(n => n !== id))} />
    })}
    {value && !bound.length && <p className="type-footnote text-muted-foreground">{t("manage:dispatch.reason.no_bindings")}</p>}
    {error && <p role="alert" className="type-footnote text-destructive">{error}</p>}
  </div>
}

function TeamEditor({ team, scope, onSaved, onCancel }: { team?: BotTeam; scope: ModuleScope; onSaved: (id: string) => void; onCancel: () => void }) {
  const t = useT()
  const id = useId()
  const [name, setName] = useState(team?.name ?? "")
  const [members, setMembers] = useState<TeamMember[]>(team?.members ?? [])
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")
  const bots = [...new Set([...Object.keys(st.bots).filter(k => !st.bots[k].main), ...members.map(m => m.bot)])]
  const save = async () => {
    setBusy(true); setError("")
    try {
      const created = await modReq<BotTeam>(team ? "PUT" : "POST", team ? `${teamsPath}/${encodeURIComponent(team.id)}` : teamsPath, { name: name.trim(), members }, undefined, scope)
      onSaved(created.id)
    } catch (e) { setError(msg(e)) } finally { setBusy(false) }
  }
  return <Card className="flex flex-col gap-3 p-4" data-team-editor>
    <label htmlFor={id} className="type-label">{t("manage:dispatch.teamName")}</label>
    <TextField id={id} value={name} maxLength={80} onChange={e => setName(e.currentTarget.value)} disabled={busy} />
    {bots.map(k => {
      const member = members.find(m => m.bot === k)
      return <div key={k} className="flex flex-col gap-2 border-t pt-2">
        <Checkbox label={botName(k)} data-team-bot={k} checked={!!member} disabled={busy}
          onChange={e => setMembers(e.currentTarget.checked ? [...members, { bot: k, nodes: null }] : members.filter(m => m.bot !== k))} />
        {member && <TeamNodeRange bot={k} nodes={member.nodes} disabled={busy} update={nodes => setMembers(members.map(m => m.bot === k ? { bot: k, nodes } : m))} />}
      </div>
    })}
    {error && <p role="alert" className="type-footnote text-destructive">{error}</p>}
    <div className="flex flex-wrap gap-2">
      <Button size="sm" disabled={busy || !name.trim() || !members.length} data-save-team onClick={() => void save()}>{t("common:action.save")}</Button>
      <Button size="sm" variant="ghost" disabled={busy} onClick={onCancel}>{t("common:action.cancel")}</Button>
    </div>
  </Card>
}

export function TeamsSection() {
  const t = useT(), load = useBotTeams()
  const { value } = load
  const [edit, setEdit] = useState<string | null>(null)
  const [busy, setBusy] = useState(false), [error, setError] = useState("")
  if (!value) return load.error ? <p role="alert">{load.error}</p> : null
  const setDefault = async (team: string) => {
    setBusy(true); setError("")
    try { await modReq("PUT", `${teamsPath}/default`, { team }, undefined, load.scope); load.reload() }
    catch (e) { setError(msg(e)) } finally { setBusy(false) }
  }
  const current = value.teams.find(o => o.id === edit)
  return <Section label={t("manage:dispatch.teams")} note={t("manage:dispatch.teamHint")}
    right={value.editable && <Button size="xs" variant="outline" data-new-team onClick={() => setEdit("")}><PlusIcon />{t("manage:dispatch.newTeam")}</Button>}>
    <Card className="flex flex-col gap-3 p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="type-label">{t("manage:dispatch.defaultTeam")}</span>
        <SetSelect testId="default-team" label={t("manage:dispatch.defaultTeam")} value={value.default ?? NO_TEAM} disabled={busy || !value.editable}
          options={[{ v: NO_TEAM, l: t("manage:dispatch.noTeam") }, ...value.teams.map(v => ({ v: v.id, l: v.name }))]}
          onChange={v => void setDefault(v)} />
      </div>
      {value.teams.map(team => <div key={team.id} className="flex flex-wrap items-center gap-2 border-t pt-2">
        <span className="type-callout font-medium">{team.name}</span>
        <span className="min-w-0 flex-1 basis-24 type-footnote text-muted-foreground">{team.members.map(m => botName(m.bot)).join(" · ")}</span>
        {value.editable && <Button size="xs" variant="ghost" data-edit-team={team.id} onClick={() => setEdit(team.id)}>{t("common:action.edit")}</Button>}
      </div>)}
      {error && <p role="alert" className="type-footnote text-destructive">{error}</p>}
    </Card>
    {edit !== null && (edit === "" || current) && <TeamEditor key={edit} team={current} scope={load.scope}
      onSaved={() => { setEdit(null); load.reload() }} onCancel={() => setEdit(null)} />}
  </Section>
}

export function DispatchDetails({ dispatch, bot }: { dispatch: NonNullable<RItemDetail["dispatch"]>; bot: string }) {
  const t = useT()
  const { team, placement: p } = dispatch
  if (!team && !p) return null
  const reason = p?.reason
  return <div className="mt-3 flex flex-col gap-1 border-t pt-2 type-footnote text-muted-foreground" data-item-dispatch data-dispatch-reason={reason}>
    <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
      <Badge>{team?.name ?? t("manage:dispatch.noTeam")}</Badge>
      {team && <span>{t(`manage:dispatch.source.${team.source}`)}</span>}
      <span>· {botName(p?.bot ?? bot)}</span>
      {p?.node && <span>· {p.node_name || (p.node === "local" ? t("manage:dispatch.local") : p.node === "unavailable" ? t("manage:dispatch.unavailable") : p.node)}</span>}
    </div>
    <p>{reason ? reasonLabel(t, reason) : t("manage:dispatch.awaiting")}</p>
    {p?.skipped && p.skipped.length > 0 && <p>{t("manage:dispatch.skipped", { reasons: [...new Set(p.skipped)].map(r => reasonLabel(t, r)).join(" · ") })}</p>}
  </div>
}
