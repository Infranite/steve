/* Device work comes first; monitoring and configuration keep their own evidence and identity.
 * Pairing remains the owner Node API’s responsibility. */
import { RuntimeSetupPanel } from "@/runtime-setup"
import { useEffect, useState } from "react"
import { cn } from "cn"
import { ChevronRightIcon, FolderIcon, LinkIcon, MonitorSmartphoneIcon, PlusIcon, RefreshCwIcon, ServerIcon, SquareTerminalIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Meter, Panel, PanelFoot, PanelRow, Tag, type Tone } from "@/panel"
import { Spinner } from "@/components/ui/spinner"
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { st, toast, refresh, tm } from "@/store"
import { api, SESSIONS_MAX, type RDevice, type RNode, type RPairing } from "@/api"
import { Input } from "@/components/ui/input"
import { NumberField } from "@/components/ui/pickers"
import { Textarea } from "@/components/ui/textarea"
import { go, Page, Meta, Empty, closeSheet, openSheet } from "@/ui"
import { OwnerChip } from "@/people/owner-chip"
import { VitalsSection, VitalsStrip } from "@/devices-vitals"
import { NodeTags } from "@/devices-tags"
import { NodeContext } from "@/devices-context"
import { NodeUpgradePanel } from "@/devices-upgrade"
import { CODE, TailcatSection } from "@/devices-tailcat"
import { t, useT } from "@/i18n"
import { hm } from "@/i18n/format"
import { useHubAddress } from "@/hub-ui"
import { homeTilde } from "@/resume"

import { WorkCounts, DeviceWorkSection } from "@/devices-work"
import { DeviceConfiguration } from "@/devices-config"
const stateText = (d: RDevice) => d.status === "暂停接活" ? t("device:paused") : t("device:configured")

/** Connection/permissions can change without a session event. Keep the last successful
 * inventory snapshot, but failed refreshes must disable configuration until a read succeeds. */
function useDeviceInventory() {
  const [devices, setDevices] = useState<RDevice[] | null>(null), [error, setError] = useState("")
  useEffect(() => {
    let live = true, timer: ReturnType<typeof setTimeout> | undefined
    const load = async () => {
      try { const r = await api.devices(); if (live) { setDevices(r.devices); setError("") } }
      catch (e) { if (live) setError((e as Error).message) }
      finally { if (live) timer = setTimeout(() => void load(), 10_000) }
    }
    void load()
    return () => { live = false; clearTimeout(timer) }
  }, [])
  return { devices: devices ?? st.devices, error }
}

/** Node registration is separate from the legacy device inventory. Only the owner's Node API
 * establishes its visible identities and current transport links; Hub harnesses are not Node caps. */
function useRemoteNodes() {
  const [nodes, setNodes] = useState<RNode[] | null>(null), [off, setOff] = useState(false)
  const [error, setError] = useState("")
  const [loading, setLoading] = useState(true)
  const [revision, setRevision] = useState(0)
  useEffect(() => {
    let live = true, busy = false, timer: ReturnType<typeof setTimeout> | undefined
    const load = async () => {
      if (!live || busy) return
      if (document.hidden) { timer = setTimeout(() => void load(), 10_000); return }
      busy = true; setLoading(true)
      try {
        const r = await api.nodes()
        if (live) { setNodes([...new Map(r.nodes.filter(n => !n.local).map(n => [n.node_id, n])).values()]); setOff(!!r.off); setError("") }
      } catch (e) { if (live) setError((e as Error).message) }
      finally { if (live) { busy = false; setLoading(false); timer = setTimeout(() => void load(), 10_000) } }
    }
    const visible = () => { if (!document.hidden && !busy) { clearTimeout(timer); void load() } }
    void load()
    document.addEventListener("visibilitychange", visible)
    return () => { live = false; clearTimeout(timer); document.removeEventListener("visibilitychange", visible) }
  }, [revision])
  return { nodes, off, error, loading, reload: () => setRevision(x => x + 1) }
}

/** a Node's link at a glance: green linked, orange waiting or paused, grey otherwise */
const NodeDot = ({ node, stale }: { node: RNode; stale: boolean }) => <span aria-hidden className={cn("size-2 shrink-0 rounded-full", stale || node.state === "revoked" ? "bg-muted-foreground" : node.state === "pending" || node.state === "paused" ? "bg-attention" : node.linked ? "bg-success" : "bg-muted-foreground")} />

/** a role the Hub records for a device: "Hub" as it is, the runner role (seeded in Chinese) in the UI language */
const roleLabel = (r: string) => (r === "执行节点" ? t("shell:devices.roleRunner") : r)
export function DevicesView() {
  const t = useT()
  const { nodes, off, error, loading, reload } = useRemoteNodes()
  const [selected, setSelected] = useState<string | null>(null)
  const ids = new Set(nodes?.map(n => n.node_id))
  const inventory = useDeviceInventory()
  const ds = inventory.devices, hub = ds.find((d) => d.roles.includes("Hub")), rest = ds.filter((d) => d !== hub && (!d.node_id || !ids.has(d.node_id)))
  const node = nodes?.find(n => n.node_id === selected)
  const connection = (n: RNode) => error ? t("shell:devices.connectionUnknown") : n.linked ? t("shell:devices.linked") : t("shell:devices.disconnected")
  const registration = (n: RNode) => error ? t("shell:devices.registrationPrevious", { state: t(`shell:devices.nodeState.${n.state}`) }) : t(`shell:devices.nodeState.${n.state}`)
  const regTone = (n: RNode): Tone => error || n.state === "revoked" ? "muted" : n.state === "pending" || n.state === "paused" ? "wait" : "ok"
  return (
    <>
    <div className="device-page"><Page title={t("shell:devices.title")} sub={t("shell:devices.sub")} max={760}
      actions={<Button size="sm" variant="outline" onClick={() => openSheet("pair")} data-pair><PlusIcon />{t("shell:devices.add")}</Button>}>
      <div className="flex flex-col gap-4">
        {inventory.error && <p role="alert">{t("device:factsFailed", { error: inventory.error })}</p>}
        {hub && <Panel k="hub" icon={<ServerIcon />} data-device-row={hub.name}
          title={<span className="inline-flex items-center gap-2">{hub.name}<Tag className="h-5 px-1.5">Hub</Tag></span>}
          sub={<span className="break-all">{hub.id} · {hub.node_id || t("device:unmapped")}</span>}
          right={<><Badge tone="neutral" data-hub-state>{hub.connected === false ? t("device:offline") : hub.node_id ? stateText(hub) : t("device:unknown")}</Badge><OwnerChip objectType="device" objectId={hub.id} bare /></>}>
          <WorkCounts id={hub.id} />{hub.running != null && hub.waiting != null && <RoomMeter running={hub.running} max={hub.max_sessions} waiting={hub.waiting} />}{hub.node_id && <VitalsStrip node={hub.node_id} />}
          <PanelFoot meta={<>{hub.node_id ? t("device:unknownEligibility") : t("device:unmapped")}</>}>
            <Button size="sm" variant="outline" onClick={() => go("#/devices/" + hub.id)} data-device-open>{t("shell:devices.details")}<ChevronRightIcon /></Button>
          </PanelFoot>
        </Panel>}
        <Panel k="others" icon={<MonitorSmartphoneIcon />} title={nodes !== null && !error ? t("shell:devices.others", { count: rest.length + nodes.length }) : t("shell:devices.othersTitle")} sub={t("shell:devices.othersSub")}
          right={<Button size="xs" variant="ghost" className="text-muted-foreground" disabled={loading} onClick={reload} aria-label={t("shell:devices.refreshNodes")} data-refresh-nodes>{loading ? <Spinner className="size-3" /> : <RefreshCwIcon />}<span className="max-sm:hidden">{t("shell:devices.refreshNodes")}</span></Button>}>
          {off && <div className="px-5 pb-3"><p className="rounded-lg bg-muted/60 px-3 py-2 type-callout text-muted-foreground" data-nodes-off>{t("shell:devices.nodesOff")}</p></div>}
          {(rest.length > 0 || !!nodes?.length) && <div className="anim-in py-1.5">{rest.map((d) => <PanelRow key={d.id} title={d.name} sub={<><span>{d.roles.map(roleLabel).join(" · ")}</span><WorkCounts id={d.id} /></>} onClick={() => go("#/devices/" + d.id)} controls><OwnerChip objectType="device" objectId={d.id} bare /><ChevronRightIcon className="size-4 text-tertiary" /></PanelRow>)}
            {nodes?.map(n => <PanelRow key={n.node_id} data-node-row={n.node_id} onClick={() => setSelected(n.node_id)}
              title={<span className="inline-flex items-center gap-2"><NodeDot node={n} stale={!!error} />{n.name}</span>}
              sub={<Meta><span>{connection(n)}</span>{!n.linked && n.link_error && <span className="line-clamp-1 text-destructive" title={n.link_error.error}>{n.link_error.error}</span>}<span className="font-mono">{n.node_id}</span></Meta>}><Tag tone={regTone(n)}>{registration(n)}</Tag><ChevronRightIcon className="size-4 text-tertiary" /></PanelRow>)}
          </div>}
          {error && <p role="alert" className="px-5 py-3 type-callout text-destructive" data-node-error>{t("shell:devices.nodesUnavailable")} · {error}</p>}
          {nodes === null && !error && <p className="px-5 py-4 type-callout text-muted-foreground" role="status" data-nodes-loading>{t("shell:devices.loadingNodes")}</p>}
          {nodes !== null && !error && !rest.length && !nodes.length && <div className="anim-in border-t-0! px-5 pt-1 pb-5" data-no-others><div className="type-body text-foreground">{t("shell:devices.noOthers")}</div><div className="mt-0.5 type-callout text-muted-foreground">{t("shell:devices.noOthersSub")}</div></div>}
        </Panel>
      </div>
    </Page></div>
    {selected && <Dialog open onOpenChange={open => !open && setSelected(null)}><DialogContent className="device-evidence max-h-[85vh] overflow-y-auto rounded-2xl sm:max-w-[560px]" data-node-detail>
      <DialogHeader><DialogTitle>{node?.name ?? t("shell:devices.remoteNode")}</DialogTitle><DialogDescription>{t("shell:devices.nodeDetailSub")}</DialogDescription></DialogHeader>
      {node ? <Panel k="node">
        <PanelRow title={t("shell:devices.registrationState")}><Tag tone={regTone(node)}>{registration(node)}</Tag></PanelRow>
        <PanelRow title={t("shell:devices.connection")}><span className="type-callout text-muted-foreground">{connection(node)}</span></PanelRow>
        <PanelRow title={t("shell:devices.nodeId")} sub={<span className="font-mono break-all select-all whitespace-normal">{node.node_id}</span>} />
        <PanelRow title={t("shell:devices.fingerprint")} sub={<span className="font-mono break-all select-all whitespace-normal">{node.fingerprint}</span>} />
        {node.last_seen != null && <PanelRow title={t("shell:devices.lastConnected")}><span className="type-callout tabular-nums text-muted-foreground">{tm(node.last_seen)}</span></PanelRow>}
        {!node.linked && node.link_error && <PanelRow title={t("shell:devices.linkError")} sub={<span className="break-all whitespace-normal text-destructive" data-node-link-error>{tm(node.link_error.at)} · {node.link_error.error}</span>} />}
      </Panel> : <p role="status">{t("shell:devices.nodeNotVisible")}</p>}
      {node && node.max_sessions != null && <Panel k="node-sessions" icon={<SquareTerminalIcon />} title={t("shell:devices.limit")} sub={t("shell:devices.limitSub", { count: node.max_sessions })}
        right={<RoomMeter running={node.running} max={node.max_sessions} waiting={node.waiting} />}>
        <div className="py-1.5"><SessionLimit max={node.max_sessions} editable={node.state !== "revoked"} save={(n) => api.nodeLimit(node.node_id, n).then(reload)} /></div>
      </Panel>}
      {node && <NodeUpgradePanel node={node} reload={reload} />}
      {node && <NodeTags node={node.node_id} />}
      {node && node.state !== "revoked" && <NodeContext node={node.node_id} value={node.owner_context ?? null} />}
      {node && <><p>{t("device:unknownEligibility")}</p><p>{t("device:unknownCapacity")}</p><VitalsSection node={node.node_id} /></>}
    </DialogContent></Dialog>}
    </>
  )
}

export function DeviceView({ id }: { id: string }) {
  const t = useT()
  const inventory = useDeviceInventory()
  const d = inventory.devices.find(x => x.id === id)
  if (!d) return <Page title={t("shell:devices.title")} back={["#/devices", t("shell:devices.title")]}><Empty title={t("shell:devices.notFound")} /></Page>
  const local = d.node_id === "local", dirs = st.projects.filter(p => p.device_id === d.id)

  return <div className="device-page"><Page title={d.name} back={["#/devices", t("shell:devices.title")]} max={1120}
    sub={<span className="break-all">{d.id}{d.node_id && <> · {d.node_id}</>}</span>}>
    <div className="flex min-w-0 flex-col gap-4">
      {inventory.error && <p role="alert">{t("device:factsFailed", { error: inventory.error })}</p>}
      <Panel k="device-state" title={t("device:connection")} sub={inventory.error ? t("device:unknown") : st.down ? t("device:offline") : d.connected === true ? t("shell:devices.linked") : d.connected === false ? t("device:offline") : t("device:unmapped")}>
        <div className="p-5"><p>{t("device:accepting")} · {d.can_configure ? stateText(d) : t("device:unknown")}</p><p>{t("device:unknownEligibility")}</p><WorkCounts id={d.id} /><p className="mt-3 text-muted-foreground" data-node-room>{d.running != null && d.waiting != null ? t("device:nodeOccupancy", { running: d.running, max: d.max_sessions, waiting: d.waiting }) : t("device:unknownCapacity")}</p></div>
      </Panel>
      <div className="device-layout"><DeviceWorkSection id={d.id} />{d.node_id ? <VitalsSection node={d.node_id} identity={`${d.name} · ${d.id}`} /> : <Panel title={t("device:monitor")} sub={t("device:unmapped")} />}</div>
      <details className="device-capabilities" data-capabilities><summary>{t("device:configuration")}</summary>
        <DeviceConfiguration key={d.id} device={d} available={!inventory.error} />
        {local ? <>
        <RuntimeSetupPanel>{local && d.can_configure && !st.down && <AddHarness />}</RuntimeSetupPanel>

          <NodeTags node="local" />
          <NodeContext node="local" />
        </> : <p>{t("device:unsupported")}</p>}
        <Panel k="projects" icon={<FolderIcon />} title={t("shell:devices.projectsTitle")}>
          {dirs.length ? dirs.map(p => <PanelRow key={p.id} title={p.name} sub={<span className="break-all font-mono">{homeTilde(p.path)}</span>} onClick={() => go("#/projects/" + p.id)} />) : <p className="p-5 text-muted-foreground">{t("shell:devices.noProjects")}</p>}
        </Panel>
        <p className="p-5">{d.os} · {d.version}</p>
        <Panel k="connection" icon={<LinkIcon />} title={t("shell:devices.connection")} sub={t("shell:devices.ownerSub")} right={<OwnerChip objectType="device" objectId={d.id} bare />} />
      </details>
      <a className="underline" href="#/settings/hub">{t("device:hub")}</a>
    </div>
  </Page></div>
}

/** how full a device is: running against its limit, and what waits for room */
function RoomMeter({ running, max, waiting }: { running?: number; max: number; waiting?: number }) {
  const t = useT()
  if (running == null || waiting == null) return <span>{t("device:unknownCapacity")}</span>
  return <span className="flex flex-col items-end gap-1.5" data-running={running} data-max={max} data-waiting={waiting}>
    <span className="type-footnote tabular-nums text-muted-foreground">{t("shell:devices.runningOf", { n: running, max })}{waiting > 0 && <span className="text-attention"> · {t("shell:devices.waitingN", { count: waiting })}</span>}</span>
    <Meter value={max ? running / max : 0} tone={running >= max ? "wait" : "run"} className="w-20" />
  </span>
}

/** Saved scheduling capacity, not a permission setting. Lowering it never interrupts existing turns. */
function SessionLimit({ max, editable, save }: { max: number; editable: boolean; save: (n: number) => Promise<unknown> }) {
  const t = useT()
  const [n, setN] = useState<number | null>(max), [busy, setBusy] = useState(false)
  useEffect(() => setN(max), [max])
  const valid = n != null && Number.isInteger(n) && n >= 1 && n <= SESSIONS_MAX, dirty = valid && n !== max
  const submit = async () => {
    if (!dirty || busy || n == null) return
    setBusy(true)
    try { await save(n); toast(t("shell:devices.limitSaved", { count: n })) } catch (e) { toast((e as Error).message) } finally { setBusy(false) }
  }
  return <div data-session-limit={max}>
    <PanelRow title={t("shell:devices.limit")} sub={t("shell:devices.limitRange", { max: SESSIONS_MAX })} className="flex-wrap gap-y-2">
      {editable ? <form className="flex shrink-0 items-center gap-2" onSubmit={(e) => { e.preventDefault(); void submit() }}>
        <NumberField size="sm" min={1} max={SESSIONS_MAX} step={1} value={n} onValueChange={setN} error={!valid ? t("shell:devices.limitInvalid", { max: SESSIONS_MAX }) : undefined} disabled={busy} aria-label={t("shell:devices.limit")} aria-invalid={!valid} className="w-32" data-session-limit-input />
        <Button size="xs" type="submit" variant={dirty ? "default" : "outline"} disabled={!valid || busy} data-session-limit-save>{busy ? <Spinner className="size-3" /> : null}{t("common:action.save")}</Button>
      </form> : <span className="type-callout tabular-nums text-muted-foreground">{max}</span>}
    </PanelRow>
    <p className="px-5 pb-3 type-footnote text-muted-foreground" data-limit-semantics>{t("shell:devices.limitEffect")}</p>
  </div>
}

/** what a harness adapter can do and the permission modes it offers, as its probe said */
/** declares a harness adapter of one's own in the Hub's manifest: an id, a name, its command, its arguments and variables, and
 * (advanced) its declared properties as JSON. It is used once the Hub restarts. */
function AddHarness() {
  const t = useT()
  const [open, setOpen] = useState(false), [busy, setBusy] = useState(false), [error, setError] = useState("")
  const [id, setId] = useState(""), [name, setName] = useState(""), [command, setCommand] = useState(""), [args, setArgs] = useState(""), [env, setEnv] = useState(""), [props, setProps] = useState("")
  if (!open) return <Button size="sm" variant="outline" onClick={() => setOpen(true)} data-add-harness><PlusIcon />{t("shell:devices.harness.add")}</Button>
  const save = async () => {
    setBusy(true); setError("")
    try {
      const envMap: Record<string, string> = {}
      for (const l of env.split("\n").map((x) => x.trim()).filter(Boolean)) { const i = l.indexOf("="); if (i <= 0) throw new Error(t("shell:devices.harness.badEnv", { line: l })); envMap[l.slice(0, i)] = l.slice(i + 1) }
      let properties: Record<string, unknown> | undefined
      if (props.trim()) { try { properties = JSON.parse(props) } catch { throw new Error(t("shell:devices.harness.badJson")) } }
      await api.addHarness({ id: id.trim(), name: name.trim() || undefined, command: command.trim(), args: args.split(/\s+/).filter(Boolean), env: envMap, properties })
      toast(t("shell:devices.harness.saved")); setOpen(false); await refresh()
    } catch (e) { setError((e as Error).message) } finally { setBusy(false) }
  }
  return <div className="anim-in flex flex-col gap-2 rounded-xl bg-muted/60 p-4" data-harness-form>
    <div className="mb-1 type-callout text-muted-foreground">{t("shell:devices.harness.intro")}</div>
    <Input placeholder={t("shell:devices.harness.id")} value={id} onChange={(e) => setId(e.target.value)} data-harness-id />
    <Input placeholder={t("shell:devices.harness.name")} value={name} onChange={(e) => setName(e.target.value)} />
    <Input placeholder={t("shell:devices.harness.command")} value={command} onChange={(e) => setCommand(e.target.value)} className="font-mono" data-harness-command />
    <Input placeholder={t("shell:devices.harness.args")} value={args} onChange={(e) => setArgs(e.target.value)} className="font-mono" />
    <Textarea placeholder={t("shell:devices.harness.env")} value={env} onChange={(e) => setEnv(e.target.value)} className="min-h-14 font-mono" />
    <details><summary className="cursor-pointer type-callout text-muted-foreground hover:text-foreground">{t("shell:devices.harness.advanced")}</summary>
      <Textarea placeholder='{"modes": {"ask": "default"}, "nested_sandbox": true}' value={props} onChange={(e) => setProps(e.target.value)} className="mt-1 min-h-20 font-mono" /></details>
    {error && <div className="type-callout text-attention" role="alert">{error}</div>}
    <div className="mt-1 flex gap-2"><Button size="sm" disabled={busy || !id.trim() || !command.trim()} onClick={() => void save()} data-harness-save>{busy && <Spinner className="size-3" />}{t("common:action.save")}</Button><Button size="sm" variant="ghost" onClick={() => setOpen(false)}>{t("common:action.cancel")}</Button></div>
  </div>
}

export function PairDialog() {
  const t = useT()
  /* the Hub's own address: this page may be served apart from it (A3), or by a shell's gateway on this machine */
  const hub = useHubAddress()
  const [pairing, setPairing] = useState<RPairing | null>(null)
  const [nodes, setNodes] = useState<RNode[]>([])
  const [busy, setBusy] = useState(false), [error, setError] = useState("")
  const [fingerprints, setFingerprints] = useState<Record<string, string>>({})
  const [now, setNow] = useState(Date.now())
  const reload = async () => { const r = await api.nodes(); setNodes(r.nodes.filter(n => !n.local)) }
  const act = async (f: () => Promise<void>) => {
    setBusy(true); setError("")
    try { await f() } catch (e) { setError((e as Error).message) } finally { setBusy(false) }
  }
  useEffect(() => {
    let live = true
    void api.nodes().then(r => { if (live) setNodes(r.nodes.filter(n => !n.local)) }).catch(e => { if (live) setError((e as Error).message) })
    const timer = setInterval(() => setNow(Date.now()), 1000)
    return () => { live = false; clearInterval(timer) }
  }, [])
  return (
    <Dialog open onOpenChange={(x) => !x && closeSheet()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[600px] max-h-[85vh] overflow-y-auto" data-pair-dialog>
        <DialogHeader><DialogTitle className="type-title">{t("shell:devices.add")}</DialogTitle><DialogDescription className="type-callout">{t("shell:devices.pairHelp")}</DialogDescription></DialogHeader>
        {error && <p role="alert" className="type-callout text-destructive">{error}</p>}
        <Button className="justify-self-start self-start max-sm:w-full max-sm:justify-self-stretch" disabled={busy || !!pairing && now < pairing.expires} onClick={() => void act(async () => { setPairing(await api.createPairing()) })}>{t("shell:devices.createCode")}</Button>
        {pairing && <div className="anim-in space-y-2 rounded-xl bg-muted/60 p-4 type-callout">
          {now < pairing.expires ? <><p className="text-muted-foreground">{t("shell:devices.codeExpires", { time: hm(pairing.expires) })}</p><code className={CODE}>{pairing.code}</code></> : <p className="text-muted-foreground">{t("shell:devices.codeExpired")}</p>}
          {pairing.pin && <><p className="text-muted-foreground">{t("shell:devices.hubPin")}</p><code className={CODE}>{pairing.pin}</code></>}
          <p className="text-muted-foreground">{t("shell:devices.setupCommand")}</p><code className={CODE} data-setup-command>steve setup node --hub {hub}{pairing.pin ? ` --pin ${pairing.pin}` : ""}</code>
          <p className="text-muted-foreground">{t("shell:devices.pairCommand")}</p><code className={CODE} data-pair-command>steve-node pair {hub}{pairing.pin ? ` --pin ${pairing.pin}` : ""}</code>
        </div>}
        <TailcatSection />
        {(pairing || nodes.length > 0) && <div className="-mb-1 flex items-center justify-between gap-3">
          <span className="type-label text-tertiary">{t("shell:devices.othersTitle")}</span>
          <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void act(reload)} data-pair-refresh><RefreshCwIcon />{t("shell:devices.refreshNodes")}</Button>
        </div>}
        {nodes.map(n => <div key={n.node_id} className="anim-in space-y-2 rounded-xl bg-muted/60 p-4 type-callout">
          <p className="type-body font-medium">{n.name} <span className="font-normal text-muted-foreground">· {t(`shell:devices.nodeState.${n.state}`)} · {n.linked ? t("shell:devices.linked") : t("shell:devices.disconnected")}</span></p>
          <code className="block break-all type-code text-muted-foreground">{n.node_id}</code><code className="block break-all type-code text-muted-foreground">{n.fingerprint}</code>
          {n.state === "pending" && <>
            <label className="block text-muted-foreground" htmlFor={`fingerprint-${n.node_id}`}>{t("shell:devices.checkFingerprint")}</label>
            <Input id={`fingerprint-${n.node_id}`} className="font-mono" value={fingerprints[n.node_id] ?? ""} onChange={e => setFingerprints(v => ({ ...v, [n.node_id]: e.target.value }))} autoComplete="off" spellCheck={false} />
            <Button disabled={busy || !n.fingerprint || fingerprints[n.node_id]?.trim() !== n.fingerprint} onClick={() => void act(async () => { await api.confirmNode(n.node_id); await reload() })}>{t("shell:devices.confirmFingerprint")}</Button>
          </>}
        </div>)}
      </DialogContent>
    </Dialog>
  )
}
