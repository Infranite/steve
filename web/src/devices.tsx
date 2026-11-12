/* 设备与远程 Node 配对；身份与确认由所有者 API 校验。列表和详情都是面板（panel.tsx）：Hub 一张带设备状态数字条，其他设备一张；
 * 详情页从设备状态起，依次是会话、运行环境、标签、项目目录、中心节点和连接。 */
import { useEffect, useState } from "react"
import { cn } from "cn"
import { ActivityIcon, ChevronRightIcon, CpuIcon, FolderIcon, LinkIcon, MonitorSmartphoneIcon, PauseIcon, PlayIcon, PlusIcon, RefreshCwIcon, ServerIcon, SquareTerminalIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Meter, Panel, PanelFoot, PanelRow, StatStrip, Tag, type Tone } from "@/panel"
import { Spinner } from "@/components/ui/spinner"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { st, toast, refresh, tm } from "@/store"
import { api, type RDevice, type RNode, type RPairing, type RRuntime } from "@/api"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { go, Page, Meta, Empty, Status, closeSheet, openSheet } from "@/ui"
import { OwnerChip } from "@/people/owner-chip"
import { VitalsSection, VitalsStrip } from "@/devices-vitals"
import { NodeTags } from "@/devices-tags"
import { TailcatSection } from "@/devices-tailcat"
import { t, useT } from "@/i18n"
import { useHubAddress } from "@/hub-ui"

const onDevice = (id: string) => Object.keys(st.S).filter((k) => st.S[k].device === id && !st.S[k].ro)
const stateText = (d: RDevice) => (d.status === "暂停接活" ? t("shell:devices.onlinePaused") : t("shell:devices.online"))

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
  const ds = st.devices, hub = ds.find((d) => d.roles.includes("Hub")), rest = ds.filter((d) => d !== hub && !ids.has(d.id))
  const node = nodes?.find(n => n.node_id === selected)
  const connection = (n: RNode) => error ? t("shell:devices.connectionUnknown") : n.linked ? t("shell:devices.linked") : t("shell:devices.disconnected")
  const registration = (n: RNode) => error ? t("shell:devices.registrationPrevious", { state: t(`shell:devices.nodeState.${n.state}`) }) : t(`shell:devices.nodeState.${n.state}`)
  const regTone = (n: RNode): Tone => error || n.state === "revoked" ? "muted" : n.state === "pending" || n.state === "paused" ? "wait" : "ok"
  const hubSessions = hub ? onDevice(hub.id).filter((k) => st.S[k].st === "run").length : 0
  return (
    <>
    <Page title={t("shell:devices.title")} sub={t("shell:devices.sub")} max={760}
      actions={<Button size="sm" variant="outline" onClick={() => openSheet("pair")} data-pair><PlusIcon />{t("shell:devices.add")}</Button>}>
      <div className="flex flex-col gap-4">
        {hub && <Panel k="hub" icon={<ServerIcon />} data-device-row={hub.name}
          title={<span className="inline-flex items-center gap-2">{hub.name}<span className="font-mono text-[11px] font-normal text-muted-foreground">Hub</span></span>}
          sub={<Meta><span>{hub.roles.filter((r) => r !== "Hub").map(roleLabel).join(" · ") || "Hub"}</span>{hub.version && <span className="font-mono" data-hub-version>{hub.version}</span>}<span>{st.runtimes.filter((r) => r.ok).map((r) => r.name).join(t("shell:list.sep")) || t("shell:devices.noHarness")}</span></Meta>}
          right={<><Tag tone={hub.status === "暂停接活" ? "wait" : "ok"} data-hub-state><span className="size-1.5 rounded-full bg-current" />{stateText(hub)}</Tag><OwnerChip objectType="device" objectId={hub.id} bare /></>}>
          <VitalsStrip node="local" />
          <PanelFoot meta={<>{t("shell:devices.whereSub")}{hubSessions > 0 && <> · {t("shell:devices.running", { count: hubSessions })}</>}</>}>
            <Button size="sm" variant="outline" onClick={() => go("#/devices/" + hub.id)} data-device-open>{t("shell:devices.details")}<ChevronRightIcon /></Button>
          </PanelFoot>
        </Panel>}
        <Panel k="others" icon={<MonitorSmartphoneIcon />} title={nodes !== null && !error ? t("shell:devices.others", { count: rest.length + nodes.length }) : t("shell:devices.othersTitle")} sub={off ? t("shell:devices.nodesOff") : t("shell:devices.othersSub")}
          right={<Button size="xs" variant="ghost" className="text-muted-foreground" disabled={loading} onClick={reload} data-refresh-nodes>{loading ? <Spinner className="size-3" /> : <RefreshCwIcon />}{t("shell:devices.refreshNodes")}</Button>}>
          {(rest.length > 0 || !!nodes?.length) && <div className="py-1.5">{rest.map((d) => <PanelRow key={d.id} title={d.name} sub={d.roles.map(roleLabel).join(" · ")} onClick={() => go("#/devices/" + d.id)} controls><OwnerChip objectType="device" objectId={d.id} bare /><ChevronRightIcon className="size-4 text-tertiary" /></PanelRow>)}
            {nodes?.map(n => <PanelRow key={n.node_id} data-node-row={n.node_id} onClick={() => setSelected(n.node_id)}
              title={<span className="inline-flex items-center gap-2"><NodeDot node={n} stale={!!error} />{n.name}</span>}
              sub={<Meta><span>{connection(n)}</span>{!n.linked && n.link_error && <span className="line-clamp-1 text-destructive" title={n.link_error.error}>{n.link_error.error}</span>}<span className="font-mono">{n.node_id}</span></Meta>}><Tag tone={regTone(n)}>{registration(n)}</Tag><ChevronRightIcon className="size-4 text-tertiary" /></PanelRow>)}
          </div>}
          {error && <p role="alert" className="px-5 py-3 type-callout text-destructive" data-node-error>{t("shell:devices.nodesUnavailable")} · {error}</p>}
          {nodes === null && !error && <p className="px-5 py-4 type-callout text-muted-foreground" role="status" data-nodes-loading>{t("shell:devices.loadingNodes")}</p>}
          {nodes !== null && !error && !rest.length && !nodes.length && <div className="px-5 py-6"><div className="type-callout text-foreground">{t("shell:devices.noOthers")}</div><div className="mt-0.5 type-footnote text-muted-foreground">{t("shell:devices.noOthersSub")}</div></div>}
          <PanelFoot meta={t("shell:devices.pairDesc")}><Button size="sm" variant="outline" onClick={() => openSheet("pair")}><PlusIcon />{t("shell:devices.add")}</Button></PanelFoot>
        </Panel>
      </div>
    </Page>
    {selected && <Dialog open onOpenChange={open => !open && setSelected(null)}><DialogContent className="max-h-[85vh] overflow-y-auto rounded-2xl sm:max-w-[560px]" data-node-detail>
      <DialogHeader><DialogTitle>{node?.name ?? t("shell:devices.remoteNode")}</DialogTitle><DialogDescription>{t("shell:devices.nodeDetailSub")}</DialogDescription></DialogHeader>
      {node ? <Panel k="node">
        <PanelRow title={t("shell:devices.registrationState")}><Tag tone={regTone(node)}>{registration(node)}</Tag></PanelRow>
        <PanelRow title={t("shell:devices.connection")}><span className="type-callout text-muted-foreground">{connection(node)}</span></PanelRow>
        <PanelRow title={t("shell:devices.nodeId")} sub={<span className="font-mono break-all select-all whitespace-normal">{node.node_id}</span>} />
        <PanelRow title={t("shell:devices.fingerprint")} sub={<span className="font-mono break-all select-all whitespace-normal">{node.fingerprint}</span>} />
        {node.last_seen != null && <PanelRow title={t("shell:devices.lastConnected")}><span className="font-mono text-[12.5px] tabular-nums text-muted-foreground">{tm(node.last_seen)}</span></PanelRow>}
        {!node.linked && node.link_error && <PanelRow title={t("shell:devices.linkError")} sub={<span className="break-all whitespace-normal text-destructive" data-node-link-error>{tm(node.link_error.at)} · {node.link_error.error}</span>} />}
      </Panel> : <p role="status">{t("shell:devices.nodeNotVisible")}</p>}
      {node && <NodeTags node={node.node_id} />}
      {node && <VitalsSection node={node.node_id} />}
      <DialogFooter><Button variant="outline" onClick={() => setSelected(null)}>{t("shell:devices.gotIt")}</Button></DialogFooter>
    </DialogContent></Dialog>}
    </>
  )
}

export function DeviceView({ id }: { id: string }) {
  const [busy, setBusy] = useState(false), [all, setAll] = useState(false), t = useT()
  const d = st.devices.find((x) => x.id === id)
  if (!d) return <Page title={t("shell:devices.title")} back={["#/devices", t("shell:devices.title")]}><Empty title={t("shell:devices.notFound")} /></Page>
  const isHub = d.roles.includes("Hub"), ss = onDevice(d.id).filter((k) => st.S[k].st !== "ended"), dirs = st.projects.filter((p) => p.device_id === d.id)
  const paused = d.status === "暂停接活"
  /* running first, then the rest by their last activity */
  const rank = (k: string) => (st.S[k].st === "run" ? 0 : st.S[k].st === "wait" ? 1 : 2)
  ss.sort((a, b) => rank(a) - rank(b))
  const running = ss.filter((k) => st.S[k].st === "run").length
  const shown = all ? ss : ss.slice(0, 8)
  const recheck = async () => { setBusy(true); try { await api.devices(true); await refresh(); toast(t("shell:devices.rechecked")) } catch (e) { toast((e as Error).message) } finally { setBusy(false) } }
  const pause = () => void api.patchDevice(d.id, { status: paused ? "正常" : "暂停接活" }).then(() => { toast(paused ? t("shell:devices.resumed") : t("shell:devices.paused")); return refresh() })
  const pauseButton = <Button size="sm" variant="outline" onClick={pause} data-pause-device>{paused ? <><PlayIcon />{t("shell:devices.resume")}</> : <><PauseIcon />{t("shell:devices.pause")}</>}</Button>
  const stateTag = <Tag tone={paused ? "wait" : "ok"}><span className="size-1.5 rounded-full bg-current" />{stateText(d)}</Tag>
  return (
    <Page title={d.name} back={["#/devices", t("shell:devices.title")]} max={760} sub={<Meta><span>{d.roles.map(roleLabel).join(" · ")}</span><span>{d.os}</span><span className="font-mono">{d.version}</span></Meta>}
      actions={pauseButton}>
      <div className="flex flex-col gap-4">
        {isHub ? <VitalsSection node="local" right={stateTag}>
          <PanelFoot meta={t("shell:devices.lastSeenAt", { time: tm(d.seen) })}>{pauseButton}</PanelFoot>
        </VitalsSection> : <Panel k="state" icon={<ActivityIcon />} title={t("shell:devices.vitals.title")} sub={t("shell:devices.lastSeenAt", { time: tm(d.seen) })} right={stateTag} />}
        <Panel k="sessions" icon={<SquareTerminalIcon />} title={t("shell:devices.sessionsHere", { count: ss.length })} sub={t("shell:devices.maxSessions", { count: d.max_sessions })}
          right={<span className="flex flex-col items-end gap-1.5" data-running><span className="font-mono text-[12px] leading-4 tabular-nums text-muted-foreground">{t("shell:devices.runningOf", { n: running, max: d.max_sessions })}</span><Meter value={d.max_sessions ? running / d.max_sessions : 0} tone={d.max_sessions && running >= d.max_sessions ? "wait" : "run"} className="w-20" /></span>}>
          {ss.length ? <div className="py-1.5">{shown.map((k) => { const s = st.S[k]; return <PanelRow key={k} title={s.title} sub={s.item ? t("shell:devices.sessionSub", { item: s.item, n: s.n, up: s.up }) : t("shell:devices.noItem")} onClick={() => go("#/sessions/" + k)}><Status s={s.st} sess /></PanelRow> })}</div>
            : <div className="px-5 py-4 type-callout text-muted-foreground">{t("shell:devices.noSessions")}</div>}
          {ss.length > shown.length && <PanelFoot><Button size="sm" variant="ghost" className="text-muted-foreground" onClick={() => setAll(true)} data-sessions-more>{t("shell:devices.moreSessions", { count: ss.length - shown.length })}</Button></PanelFoot>}
        </Panel>
        <Panel k="runtimes" icon={<CpuIcon />} title={t("shell:devices.runtimes")} sub={t("shell:devices.runtimesSub", { ok: st.runtimes.filter((r) => r.ok).length, count: st.runtimes.length })}
          right={<Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void recheck()}>{busy ? <Spinner className="size-3" /> : <RefreshCwIcon />}{t("shell:devices.recheck")}</Button>}>
          {st.runtimes.length ? <div className="py-1.5">{st.runtimes.map((r) => <div key={r.id} className="flex items-start gap-4 px-5 py-3 [&+&]:border-t [&+&]:border-border/60" data-runtime={r.id}>
            <div className="min-w-0 flex-1">
              <div className="flex flex-wrap items-center gap-2 type-callout text-foreground">{r.name}<span className="font-mono text-[11.5px] text-muted-foreground">{r.version || r.adapter_version}</span><span className="type-footnote text-tertiary">{r.custom ? t("shell:devices.harness.custom") : t("shell:devices.harness.builtin")}</span></div>
              <div className="mt-0.5 flex flex-col gap-0.5 type-footnote text-muted-foreground"><span className="truncate">{r.note} · <span className="font-mono">{r.path.replace(/^\/Users\/[^/]+/, "~")}</span></span>{!r.ok && r.error && <span className="text-attention" data-runtime-error>{r.error}</span>}<AdapterUpdate r={r} /><RuntimeChips r={r} /></div>
            </div>
            <Tag tone={r.ok ? "ok" : "wait"}>{r.ok ? t("shell:devices.available") : t("shell:devices.unavailable")}</Tag>
          </div>)}</div> : <div className="px-5 py-4 type-callout text-muted-foreground">{t("shell:devices.noRuntimes")}</div>}
          {isHub && <div className="px-5 py-3"><AddHarness /></div>}
        </Panel>
        {isHub && <NodeTags node="local" />}
        <Panel k="projects" icon={<FolderIcon />} title={t("shell:devices.projectsTitle")} sub={t("shell:devices.projectsSub", { count: dirs.length })}
          right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => go("#/projects")}>{t("shell:devices.manageProjects")}</Button>}>
          {dirs.length ? <div className="py-1.5">{dirs.map((p) => <PanelRow key={p.id} title={p.name} sub={<span className="font-mono">{p.path.replace(/^\/Users\/[^/]+/, "~")}</span>} onClick={() => go("#/projects/" + p.id)}><ChevronRightIcon className="size-4 text-tertiary" /></PanelRow>)}</div>
            : <div className="px-5 py-4 type-callout text-muted-foreground">{t("shell:devices.noProjects")}</div>}
        </Panel>
        {isHub && <Panel k="hub" icon={<ServerIcon />} title={t("shell:devices.hubNode")} sub={t("shell:devices.hubNodeSub")}>
          <div className="py-1.5">
            <PanelRow title={t("shell:devices.storage")} sub={t("shell:devices.storageSub")} />
            <PanelRow title={t("shell:devices.access")} sub={t("shell:devices.accessSub")} />
            <PanelRow title={t("shell:devices.models")} sub={t("shell:devices.modelsSub")} />
          </div>
        </Panel>}
        <Panel k="connection" icon={<LinkIcon />} title={t("shell:devices.connection")} sub={t("shell:devices.ownerSub")} right={<OwnerChip objectType="device" objectId={d.id} bare />}>
          <StatStrip items={[{ k: "seen", label: t("shell:devices.lastSeen"), value: tm(d.seen) }, { k: "max", label: t("shell:devices.maxShort"), value: d.max_sessions }]} />
        </Panel>
      </div>
    </Page>
  )
}

/** what a harness adapter can do and the permission modes it offers, as its probe said */
/** an adapter Steve keeps up to date: installed and latest release, as of the Node's last check */
function AdapterUpdate({ r }: { r: RRuntime }) {
  const t = useT()
  const u = r.update
  if (!u || u.policy === "off") return null
  const newer = !!u.latest && u.latest !== u.installed
  const text = u.error ? t("shell:devices.harness.updateError", { error: u.error })
    : !u.latest ? t("shell:devices.harness.updateUnchecked")
    : !newer ? t("shell:devices.harness.upToDate", { version: u.installed ?? "", at: u.checked ? tm(u.checked) : "" })
    : u.policy === "auto" ? t("shell:devices.harness.updatePending", { from: u.installed ?? "—", to: u.latest })
    : t("shell:devices.harness.updateAvailable", { to: u.latest })
  return <span className={newer || u.error ? "text-attention" : undefined} data-adapter-update>{text}</span>
}

function RuntimeChips({ r }: { r: RRuntime }) {
  const t = useT()
  const caps = r.caps
  if (!caps) return null
  const chips = ([["sessions", caps.sessions], ["judge", caps.jobs], ["images", caps.images], ["resume", caps.resume], ["history", caps.history]] as const).filter(([, on]) => on).map(([k]) => t(`shell:devices.harness.cap.${k}`))
  const modes = (r.modes ?? []).map((m) => t(`shell:devices.harness.mode.${m}`, { defaultValue: m }))
  return <span className="flex flex-wrap gap-1 pt-0.5" data-runtime-caps>{[...chips, ...modes].map((c) => <Badge key={c} variant="secondary" className="type-footnote">{c}</Badge>)}</span>
}

/** declares a harness adapter of one's own in the Hub's manifest: an id, a name, its command, its arguments and variables, and
 * (advanced) its declared properties as JSON. It is used once the Hub restarts. */
function AddHarness() {
  const t = useT()
  const [open, setOpen] = useState(false), [busy, setBusy] = useState(false), [error, setError] = useState("")
  const [id, setId] = useState(""), [name, setName] = useState(""), [command, setCommand] = useState(""), [args, setArgs] = useState(""), [env, setEnv] = useState(""), [props, setProps] = useState("")
  if (!open) return <Button size="sm" variant="outline" className="mt-2" onClick={() => setOpen(true)} data-add-harness><PlusIcon />{t("shell:devices.harness.add")}</Button>
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
  return <div className="mt-2 flex flex-col gap-2 rounded-lg border p-3" data-harness-form>
    <div className="type-footnote text-muted-foreground">{t("shell:devices.harness.intro")}</div>
    <Input placeholder={t("shell:devices.harness.id")} value={id} onChange={(e) => setId(e.target.value)} data-harness-id />
    <Input placeholder={t("shell:devices.harness.name")} value={name} onChange={(e) => setName(e.target.value)} />
    <Input placeholder={t("shell:devices.harness.command")} value={command} onChange={(e) => setCommand(e.target.value)} className="font-mono" data-harness-command />
    <Input placeholder={t("shell:devices.harness.args")} value={args} onChange={(e) => setArgs(e.target.value)} className="font-mono" />
    <Textarea placeholder={t("shell:devices.harness.env")} value={env} onChange={(e) => setEnv(e.target.value)} className="min-h-14 font-mono" />
    <details><summary className="type-footnote text-muted-foreground">{t("shell:devices.harness.advanced")}</summary>
      <Textarea placeholder='{"modes": {"ask": "default"}, "nested_sandbox": true}' value={props} onChange={(e) => setProps(e.target.value)} className="mt-1 min-h-20 font-mono" /></details>
    {error && <div className="type-footnote text-attention" role="alert">{error}</div>}
    <div className="flex gap-2"><Button size="sm" disabled={busy || !id.trim() || !command.trim()} onClick={() => void save()} data-harness-save>{busy && <Spinner className="size-3" />}{t("common:action.save")}</Button><Button size="sm" variant="ghost" onClick={() => setOpen(false)}>{t("common:action.cancel")}</Button></div>
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
        <DialogHeader><DialogTitle className="type-headline">{t("shell:devices.add")}</DialogTitle><DialogDescription className="type-callout">{t("shell:devices.pairDesc")}</DialogDescription></DialogHeader>
        {error && <p role="alert" className="type-callout text-destructive">{error}</p>}
        <p className="type-callout text-muted-foreground">{t("shell:devices.pairHelp")}</p>
        <Button disabled={busy || !!pairing && now < pairing.expires} onClick={() => void act(async () => { setPairing(await api.createPairing()) })}>{t("shell:devices.createCode")}</Button>
        {pairing && <div className="rounded-xl border p-4 space-y-2 type-callout">
          {now < pairing.expires ? <><p>{t("shell:devices.codeExpires", { time: new Date(pairing.expires).toLocaleTimeString() })}</p><code className="block break-all select-all">{pairing.code}</code></> : <p>{t("shell:devices.codeExpired")}</p>}
          {pairing.pin && <><p>{t("shell:devices.hubPin")}</p><code className="block break-all select-all">{pairing.pin}</code></>}
          <p>{t("shell:devices.setupCommand")}</p><code className="block break-all select-all" data-setup-command>steve setup node --hub {hub}{pairing.pin ? ` --pin ${pairing.pin}` : ""}</code>
          <p>{t("shell:devices.pairCommand")}</p><code className="block break-all select-all" data-pair-command>steve-node pair {hub}{pairing.pin ? ` --pin ${pairing.pin}` : ""}</code>
        </div>}
        <TailcatSection />
        <Button variant="outline" disabled={busy} onClick={() => void act(reload)}><RefreshCwIcon />{t("shell:devices.refreshNodes")}</Button>
        {nodes.map(n => <div key={n.node_id} className="rounded-xl border p-4 space-y-2 type-callout">
          <p className="font-medium">{n.name} · {t(`shell:devices.nodeState.${n.state}`)} · {n.linked ? t("shell:devices.linked") : t("shell:devices.disconnected")}</p>
          <code className="block break-all">{n.node_id}</code><code className="block break-all">{n.fingerprint}</code>
          {n.state === "pending" && <>
            <label className="block" htmlFor={`fingerprint-${n.node_id}`}>{t("shell:devices.checkFingerprint")}</label>
            <input id={`fingerprint-${n.node_id}`} className="w-full rounded-md border bg-background px-3 py-2 font-mono" value={fingerprints[n.node_id] ?? ""} onChange={e => setFingerprints(v => ({ ...v, [n.node_id]: e.target.value }))} autoComplete="off" spellCheck={false} />
            <Button disabled={busy || !n.fingerprint || fingerprints[n.node_id]?.trim() !== n.fingerprint} onClick={() => void act(async () => { await api.confirmNode(n.node_id); await reload() })}>{t("shell:devices.confirmFingerprint")}</Button>
          </>}
        </div>)}
        <DialogFooter><Button variant="outline" onClick={closeSheet}>{t("shell:devices.gotIt")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
