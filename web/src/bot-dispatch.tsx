/* Bot Team configuration and per-Bot execution grants, scoped to the workspace they were loaded in. */
import { useEffect, useId, useState, type ReactNode } from "react"
import { ArrowDownIcon, ArrowUpIcon, PlusIcon, RefreshCwIcon } from "lucide-react"
import { modReq, type ModuleScope, type RItemDetail } from "@/api"
import type { BotProposal, PlacementBlocked, PlacementExplanation, PlacementRejection } from "@/gen"
import { decisionView } from "@/data/sync"
import { hubBase } from "@/hub"
import { useT } from "@/i18n"
import { botName, refresh, setSetting, st, type Wait } from "@/store"
import { Badge } from "@/components/affect/badge-clean"
import { Checkbox } from "@/components/affect/checkbox-clean"
import { Textarea } from "@/components/ui/textarea"
import { TextField } from "@/components/affect/text-field-clean"
import { Button } from "@/components/ui/button"
import { SetSelect } from "@/settings-controls"
import { Card, Section } from "@/ui"

/** The Team choice that asks for no Team, even where the workspace has a default one. */
export const NO_TEAM = "none"
export type TeamMember = { bot: string; nodes: string[] | null }
export type BotTeam = { id: string; name: string; members: TeamMember[] }
type TeamList = { default: string | null; teams: BotTeam[]; editable: boolean }
type Candidate = { id: string; name: string | null; state: "candidate" | "pending" | "unavailable"; reason: PlacementRejection | null; checked_at: number | null }
type NodeList = { editable: boolean; bindings: string[]; candidates: Candidate[]; unavailable_bindings: number }
export type ItemDispatch = {
  team: { id: string; name: string; source: "default" | "explicit" | "inherited" } | null
  blocked?: PlacementBlocked | null
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
      e => { if (!cancel.signal.aborted) setResult({ key, error: msg(e) }) },
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
  const bots = [...new Set([...Object.keys(st.bots).filter(k => !st.bots[k].main), ...members.map(m => m.bot).filter(k => !st.bots[k]?.main)])]
  const save = async () => {
    setBusy(true); setError("")
    try {
      const created = await modReq<BotTeam>(team ? "PUT" : "POST", team ? `${teamsPath}/${encodeURIComponent(team.id)}` : teamsPath, { name: name.trim(), members }, undefined, scope)
      onSaved(created.id)
    } catch (e) { setError(msg(e)) } finally { setBusy(false) }
  }
  return <Card className="flex flex-col gap-3 p-4" data-team-editor>
    <p className="type-footnote text-muted-foreground">{t("manage:dispatch.coordinatorNote")}</p>
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

/** Share this load with the Bot cards so a Team edit updates both views together. */
export function TeamsSection({ load }: { load: ReturnType<typeof useBotTeams> }) {
  const t = useT()
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
  const mainBot = Object.values(st.bots).find(b => b.main)
  const noExecutor = st.cfg.noExecutor === "steve" ? "steve" : "ask"
  return <Section label={t("manage:dispatch.teams")} note={t("manage:dispatch.teamHint")}
    right={value.editable && <Button size="xs" variant="outline" data-new-team onClick={() => setEdit("")}><PlusIcon />{t("manage:dispatch.newTeam")}</Button>}>
    <Card className="flex flex-col gap-3 p-4">
      {mainBot && <div className="flex flex-wrap items-center gap-2" data-team-coordinator>
        <span className="type-callout font-medium">{mainBot.n}</span>
        <Badge tone="info">{t("manage:dispatch.coordinator")}</Badge>
        <span className="basis-full type-footnote text-muted-foreground">{t("manage:dispatch.coordinatorNote")}</span>
      </div>}
      <div className="flex flex-wrap items-center justify-between gap-2 border-t pt-2">
        <span className="type-label">{t("manage:dispatch.defaultTeam")}</span>
        <SetSelect testId="default-team" label={t("manage:dispatch.defaultTeam")} value={value.default ?? NO_TEAM} disabled={busy || !value.editable}
          options={[{ v: NO_TEAM, l: t("manage:dispatch.noTeam") }, ...value.teams.map(v => ({ v: v.id, l: v.name }))]}
          onChange={v => void setDefault(v)} />
      </div>
      <div className="flex flex-wrap items-center justify-between gap-2 border-t pt-2" data-no-executor-policy>
        <div className="min-w-0 flex-1 basis-48">
          <div className="type-label">{t("manage:items.noExecutor")}</div>
          <p className="type-footnote text-muted-foreground">{t("manage:items.noExecutorSub")}</p>
        </div>
        <SetSelect testId="no-executor-policy" label={t("manage:items.noExecutor")} value={noExecutor} disabled={busy || !value.editable}
          options={[{ v: "ask", l: t("manage:items.noExecutorAsk") }, { v: "steve", l: t("manage:items.noExecutorSteve") }]}
          onChange={v => void setSetting({ noExecutor: v })} />
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

export function PlacementWhy({ value, detail = false }: { value: PlacementExplanation; detail?: boolean }) {
  const t = useT()
  const skipped = [...new Set(value.rejected.map(b => b.reason))]
  if (value.reason === "coordinator_deferred") {
    const because = [
      value.fallback && reasonLabel(t, value.fallback),
      value.deferred_by && t(`manage:dispatch.deferredBy.${value.deferred_by}`),
    ].filter(Boolean).join(" · ") || t("manage:dispatch.reason.no_candidates")
    const perBot = value.rejected.map(b => b.bot ? `${botName(b.bot)}: ${reasonLabel(t, b.reason)}` : reasonLabel(t, b.reason))
    return <div className="min-w-0 break-words type-footnote text-muted-foreground" data-dispatch-reason={value.reason} data-deferred-by={value.deferred_by ?? undefined}>
      <p>{t("manage:dispatch.coordinatorBecause", { reason: because })}</p>
      {detail && perBot.length > 0 && <p>{t("manage:dispatch.skipped", { reasons: perBot.join(" · ") })}</p>}
    </div>
  }
  return <div className="min-w-0 break-words type-footnote text-muted-foreground" data-dispatch-reason={value.reason}>
    <p>{t(`manage:dispatch.selection.${value.reason}`)}{value.matched.length > 0 && <> · {value.matched.join(" · ")}</>}
      {value.fallback && <> · {reasonLabel(t, value.fallback)}</>}</p>
    {detail && skipped.length > 0 && <p>{t("manage:dispatch.skipped", { reasons: skipped.map(r => reasonLabel(t, r)).join(" · ") })}</p>}
  </div>
}

export function NoExecutorDecisionBody({ w, act, busy, head, foot }: {
  w: Wait
  act: (choice: string, text?: string) => Promise<void>
  busy: boolean
  head: (title: ReactNode) => ReactNode
  foot: (c: ReactNode) => ReactNode
}) {
  const t = useT()
  const view = decisionView(w.decision)
  const prompt = view?.question.kind === "key" && view.question.template.code === "no_executor"
    ? view.question.template
    : undefined
  const enabledBots = Object.keys(st.bots).filter(k => st.bots[k].on)
  const [picked, setPicked] = useState(() => enabledBots.find(k => !st.bots[k].main) ?? enabledBots[0] ?? "")
  const effectiveBot = enabledBots.includes(picked) ? picked : (enabledBots[0] ?? "")
  const proposal = prompt?.proposal
  return <>
    {head(w.q)}
    <div className="flex flex-col gap-3 px-4 pb-3" data-no-executor-card>
      {prompt?.team && <div className="flex items-center gap-2 type-footnote text-muted-foreground">
        <Badge>{prompt.team.name}</Badge>
      </div>}
      {prompt && prompt.items.length > 0 && <div className="flex flex-col gap-1">
        <div className="type-footnote font-medium text-foreground/80">{t("manage:dispatch.noExecutorCard.waitingItems")}</div>
        <ul className="flex flex-col gap-1" data-no-executor-items>
          {prompt.items.map(it => <li key={it.id} className="type-footnote">
            <a href={`#/items/${it.id}`} className="hover:underline"><span className="font-mono tabular-nums">{it.id}</span> {it.title}</a>
          </li>)}
        </ul>
      </div>}
      {prompt && <div className="rounded-lg bg-muted/60 p-3 type-footnote" data-no-executor-reasons>
        <div className="mb-1 font-medium text-foreground/80">{t("manage:dispatch.noExecutorCard.rejected")}</div>
        {prompt.rejected.length > 0
          ? <ul className="flex flex-col gap-1 text-muted-foreground">
              {prompt.rejected.map((b, idx) => <li key={`${b.bot ?? ""}:${b.reason}:${idx}`}>
                {b.bot ? `${botName(b.bot)}: ` : ""}{reasonLabel(t, b.reason)}
              </li>)}
            </ul>
          : <p className="text-muted-foreground">{reasonLabel(t, prompt.reason)}</p>}
      </div>}
      <div className="flex flex-col gap-2 border-t pt-3">
        <div className="type-footnote font-medium text-foreground/80">{t("manage:dispatch.noExecutorCard.assignLabel")}</div>
        {enabledBots.length > 0 ? <div className="flex flex-wrap items-center gap-2">
          <SetSelect testId="no-executor-bot" label={t("manage:dispatch.noExecutorCard.pickBot")} value={effectiveBot} disabled={busy} onChange={setPicked}
            options={enabledBots.map(k => ({ v: k, l: st.bots[k].main ? `${botName(k)} · ${t("manage:dispatch.coordinator")}` : botName(k) }))} />
          <Button size="sm" disabled={busy || !effectiveBot} data-no-executor-assign onClick={() => void act("assign_bot", effectiveBot)}>
            {t("manage:dispatch.noExecutorCard.assignAction")}
          </Button>
        </div> : <p className="type-footnote text-muted-foreground">{t("manage:dispatch.noExecutorCard.noAvailableBots")}</p>}
      </div>
      {proposal && w.options.includes("confirm_proposal") && <div className="flex flex-col gap-2 rounded-lg border p-3" data-no-executor-proposal>
        <div className="flex flex-wrap items-center justify-between gap-2">
          <span className="type-footnote font-medium">{t("manage:dispatch.proposal.title")} · {proposal.name}</span>
          <Button size="sm" variant="secondary" disabled={busy} data-no-executor-confirm onClick={() => void act("confirm_proposal", JSON.stringify({
            expected_revision: proposal.revision,
            name: proposal.name,
            role: proposal.role,
            specialties: proposal.specialties,
            harness: proposal.harness,
            model: proposal.model,
          }))}>{t("manage:dispatch.noExecutorCard.proposalAction")}</Button>
        </div>
        <p className="type-footnote text-muted-foreground">
          {[proposal.role, t("manage:dispatch.proposal.node", { node: proposal.node === "local" ? t("manage:dispatch.local") : proposal.node }), proposal.team && t("manage:dispatch.proposal.team", { team: proposal.team.name })].filter(Boolean).join(" · ")}
        </p>
      </div>}
      <p className="type-footnote text-muted-foreground">{t("manage:dispatch.noExecutorCard.laterHint")}</p>
    </div>
    {foot(<Button size="sm" variant="outline" disabled={busy} data-no-executor-later onClick={() => void act("later")}>{t("manage:dispatch.noExecutorCard.later")}</Button>)}
  </>
}

export function DispatchDetails({ dispatch, placement, unassigned }: { dispatch?: RItemDetail["dispatch"]; placement?: PlacementExplanation; unassigned?: boolean }) {
  const t = useT()
  const team = dispatch?.team, blocked = unassigned && dispatch?.blocked
  if (!team && !placement && !blocked) return null
  return <div className="mt-3 flex flex-col gap-1 border-t pt-2 type-footnote text-muted-foreground" data-item-dispatch>
    <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
      {team && <><Badge>{team.name}</Badge><span>{t(`manage:dispatch.source.${team.source}`)}</span></>}
      {placement && <span>{botName(placement.bot)}</span>}
    </div>
    {placement && <PlacementWhy value={placement} detail />}
    {blocked && <BlockedPlacement blocked={blocked} />}
  </div>
}

export function BlockedPlacement({ blocked }: { blocked: PlacementBlocked }) {
  const t = useT()
  const reasons = [...new Set(blocked.rejected.map(b => b.reason))].filter(reason => reason !== blocked.reason)
  const botSetup: PlacementRejection[] = ["bot", "no_bindings", "no_candidates", "outside_team", "unauthorized"]
  const deviceSetup: PlacementRejection[] = ["offline", "harness", "model", "permission", "signin", "session_probe", "pending", "effort"]
  const fix = botSetup.includes(blocked.reason) ? "bots" : deviceSetup.includes(blocked.reason) ? "devices" : undefined
  return <div role="alert" className="rounded-lg bg-muted p-3 type-footnote" data-placement-blocked={blocked.reason}>
    <p>{reasonLabel(t, blocked.reason)}</p>
    {reasons.map(reason => <p key={reason} className="text-muted-foreground">{reasonLabel(t, reason)}</p>)}
    {fix && <a href={`#/${fix}`} className="type-footnote underline">{t(`manage:dispatch.recover.${fix}`)}</a>}
  </div>
}

export function BotProposalCard({ proposal, onClose }: { proposal: BotProposal; onClose: () => void }) {
  const t = useT()
  const [name, setName] = useState(proposal.name), [role, setRole] = useState(proposal.role)
  const [tags, setTags] = useState(proposal.specialties.join("\n"))
  const [harness, setHarness] = useState(proposal.harness), [model, setModel] = useState(proposal.model ?? "")
  const [busy, setBusy] = useState(false), [error, setError] = useState<string>()
  const [scope] = useState(activeScope)
  const specialties = tags.split("\n").map(tag => tag.trim()).filter(Boolean)
  const valid = name.trim() && harness.trim() && specialties.length <= 12 && specialties.every(tag => tag.length <= 48)
  async function confirm() {
    setBusy(true)
    setError(undefined)
    try {
      await modReq<{ id: string }>("POST", `/bot-proposals/${encodeURIComponent(proposal.id)}/confirm`, {
        expected_revision: proposal.revision, name: name.trim(), role, specialties,
        harness: harness.trim(), model: model.trim() || null,
      }, undefined, scope)
      await refresh()
      onClose()
    } catch (e) {
      setError(msg(e))
    } finally {
      setBusy(false)
    }
  }
  return <Card className="flex flex-col gap-3 p-4" data-bot-proposal>
    <h3 className="type-label">{t("manage:dispatch.proposal.title")}</h3>
    <TextField aria-label={t("manage:form.name")} value={name} maxLength={40} onChange={e => setName(e.target.value)} />
    <TextField aria-label={t("manage:form.role")} value={role} maxLength={200} onChange={e => setRole(e.target.value)} />
    <Textarea aria-label={t("manage:form.specialties")} value={tags} onChange={e => setTags(e.target.value)} />
    <div className="grid grid-cols-2 gap-3">
      <TextField aria-label="Harness" value={harness} maxLength={80} onChange={e => setHarness(e.target.value)} />
      <TextField aria-label={t("manage:form.model")} value={model} maxLength={256} onChange={e => setModel(e.target.value)} />
    </div>
    <p className="type-footnote text-muted-foreground">{t("manage:dispatch.proposal.node", { node: proposal.node === "local" ? t("manage:dispatch.local") : proposal.node })}</p>
    <p className="type-footnote text-muted-foreground">{t("manage:dispatch.proposal.team", { team: proposal.team?.name ?? t("manage:dispatch.noTeam") })}</p>
    {error && <p role="alert" className="type-footnote text-destructive">{error}</p>}
    <div className="flex flex-wrap gap-2">
      <Button size="sm" disabled={busy || !valid} onClick={() => void confirm()}>{t("manage:dispatch.proposal.confirm")}</Button>
      <Button size="sm" variant="ghost" disabled={busy} onClick={onClose}>{t("common:action.cancel")}</Button>
    </div>
  </Card>
}
