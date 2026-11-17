/* Devices · a Node (#/devices/nodes/<id>): the Hub's own Node and a paired one on one page. Its work, monitoring and
 * configuration; for a paired Node its identity, its version and the owner's actions (stop or resume taking new work,
 * rename, unpair). An old link to a device (#/devices/<device id>) lands here by the Node its row names. */
import { RuntimeSetupPanel } from "@/runtime-setup"
import { useEffect, useState } from "react"
import { FolderIcon, LinkIcon, MoreHorizontalIcon, PauseIcon, PlayIcon, PlusIcon, SquareTerminalIcon, FingerprintIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Meter, Panel, PanelRow } from "@/panel"
import { Spinner } from "@/components/ui/spinner"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { st, toast, refresh } from "@/store"
import { api, SESSIONS_MAX, type RDevice } from "@/api"
import { Input } from "@/components/ui/input"
import { NumberField } from "@/components/ui/pickers"
import { Textarea } from "@/components/ui/textarea"
import { go, Page, Empty, Loading } from "@/ui"
import { OwnerChip } from "@/people/owner-chip"
import { VitalsSection } from "@/devices-vitals"
import { NodeTags } from "@/devices-tags"
import { NodeContext } from "@/devices-context"
import { NodeUpgradePanel } from "@/devices-upgrade"
import { useT } from "@/i18n"
import { date } from "@/i18n/format"
import { homeTilde } from "@/resume"
import { WorkCounts, DeviceWorkSection } from "@/devices-work"
import { DeviceConfiguration } from "@/devices-config"
import { agoText, findNode, nodeStatus, type NodeView } from "@/device-model"
import { useDevices } from "@/devices-data"
import { DCard, DRow, StatusBadge } from "@/devices-ui"

const BACK = "#/devices/nodes"

export function NodePage({ id }: { id: string }) {
  const t = useT(), { views, nodesRead, devicesError } = useDevices()
  const v = findNode(views, id)
  const back: [string, string] = [BACK, t("devices:node.back")]
  if (!v) return <Page title={t("devices:title")} back={back}>{nodesRead || devicesError ? <Empty title={t("devices:node.notFound")} /> : <Loading />}</Page>
  if (v.node && !v.local) return <RemoteNode v={v} />
  return v.device ? <DeviceView d={v.device} error={devicesError} /> : <Page title={v.name} back={back}><Loading /></Page>
}

/** the Hub's own Node, or a device row that names no paired Node: its work, monitoring and configuration */
function DeviceView({ d, error }: { d: RDevice; error: string }) {
  const t = useT()
  const local = d.node_id === "local", dirs = st.projects.filter(p => p.device_id === d.id)
  return <div className="device-page"><Page title={local ? t("devices:map.localNode") : d.name} back={[BACK, t("devices:node.back")]} max={1120}
    sub={<span className="break-all">{d.name}{d.os && <> · {d.os}</>}</span>}>
    <div className="flex min-w-0 flex-col gap-4" data-node-page={d.node_id ?? d.id}>
      {error && <p role="alert">{t("device:factsFailed", { error })}</p>}
      <Panel k="device-state" title={t("device:connection")} sub={error ? t("device:unknown") : st.down ? t("device:offline") : d.connected === true ? t("shell:devices.linked") : d.connected === false ? t("device:offline") : t("device:unmapped")}>
        <div className="p-5"><WorkCounts id={d.id} />{d.running != null && d.waiting != null && <p className="mt-3 text-muted-foreground" data-node-room>{t("device:nodeOccupancy", { running: d.running, max: d.max_sessions, waiting: d.waiting })}</p>}</div>
      </Panel>
      <div className="device-layout"><DeviceWorkSection id={d.id} />{d.node_id ? <VitalsSection node={d.node_id} identity={`${d.name} · ${d.id}`} /> : <Panel title={t("device:monitor")} sub={t("device:unmapped")} />}</div>
      <details className="device-capabilities" data-capabilities><summary>{t("device:configuration")}</summary>
        <DeviceConfiguration key={d.id} device={d} available={!error} />
        {local ? <>
          <RuntimeSetupPanel>{d.can_configure && !st.down && <AddHarness />}</RuntimeSetupPanel>
          <NodeTags node="local" />
          <NodeContext node="local" />
        </> : <p>{t("device:unsupported")}</p>}
        <Panel k="projects" icon={<FolderIcon />} title={t("shell:devices.projectsTitle")}>
          {dirs.length ? dirs.map(p => <PanelRow key={p.id} title={p.name} sub={<span className="break-all font-mono">{homeTilde(p.path)}</span>} onClick={() => go("#/projects/" + p.id)} />) : <p className="p-5 text-muted-foreground">{t("shell:devices.noProjects")}</p>}
        </Panel>
        <Panel k="connection" icon={<LinkIcon />} title={t("shell:devices.connection")} sub={t("shell:devices.ownerSub")} right={<OwnerChip objectType="device" objectId={d.id} bare />} />
      </details>
    </div>
  </Page></div>
}

/** a fingerprint in groups of four, as the Node's terminal prints it */
const groupsOf = (fp: string) => fp.replace(/^sha256:/, "").match(/.{1,4}/g)?.join(" ") ?? fp

/** a paired Node: what it is and how it connects, its sessions, version, tags, personal config and monitoring */
function RemoteNode({ v }: { v: NodeView }) {
  const t = useT(), { hubVersion, reloadNodes } = useDevices()
  const node = v.node!
  const [busy, setBusy] = useState(false), [renaming, setRenaming] = useState(false), [revoking, setRevoking] = useState(false)
  const status = nodeStatus(v, hubVersion)
  const act = async (f: () => Promise<unknown>, done: string) => {
    setBusy(true)
    try { await f(); toast(done); reloadNodes() } catch (e) { toast((e as Error).message) } finally { setBusy(false) }
  }
  const editable = node.state === "active" || node.state === "paused"
  return <div className="device-page"><Page title={node.name} back={[BACK, t("devices:node.back")]} max={1120}
    sub={<span className="inline-flex flex-wrap items-center gap-2"><StatusBadge status={status} />{node.last_seen != null && <span title={date(node.last_seen, { dateStyle: "medium", timeStyle: "medium" })}>{t("devices:node.lastSeen")} · {agoText(node.last_seen)}</span>}</span>}
    toolbar={editable && <>
      <Button size="sm" variant="outline" disabled={busy} onClick={() => void act(() => node.state === "paused" ? api.resumeNode(node.node_id) : api.pauseNode(node.node_id), t(node.state === "paused" ? "devices:node.resumed" : "devices:node.paused"))} data-node-pause={node.state}>
        {node.state === "paused" ? <PlayIcon /> : <PauseIcon />}{t(node.state === "paused" ? "devices:node.resume" : "devices:node.pause")}</Button>
      <DropdownMenu>
        <DropdownMenuTrigger render={<Button size="icon-sm" variant="outline" aria-label={t("devices:node.more")} data-node-more />}><MoreHorizontalIcon /></DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem onClick={() => setRenaming(true)} data-node-rename>{t("devices:node.rename")}</DropdownMenuItem>
          <DropdownMenuItem variant="destructive" onClick={() => setRevoking(true)} data-node-revoke>{t("devices:node.revoke")}</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </>}>
    <div className="flex min-w-0 flex-col gap-4" data-node-detail={node.node_id}>
      {!node.linked && node.state !== "pending" && node.state !== "revoked" && <p className="rounded-xl bg-muted/60 px-4 py-3 type-callout text-muted-foreground" data-node-offline>{t("devices:node.offlineNote")}</p>}
      <DCard icon={<FingerprintIcon />} title={t("devices:node.identity")} data-node-identity>
        <dl className="grid gap-x-6 gap-y-2.5 px-5 pb-5 sm:grid-cols-[9rem_minmax(0,1fr)]">
          <dt className="type-callout text-muted-foreground">{t("devices:node.nodeId")}</dt><dd className="type-code break-all select-all">{node.node_id}</dd>
          <dt className="type-callout text-muted-foreground">{t("devices:node.fingerprint")}</dt><dd className="type-code break-all select-all" data-node-fingerprint={node.fingerprint}>{groupsOf(node.fingerprint)}</dd>
          {node.last_seen != null && <><dt className="type-callout text-muted-foreground">{t("devices:node.lastSeen")}</dt><dd className="type-callout tabular-nums" title={date(node.last_seen, { dateStyle: "medium", timeStyle: "medium" })}>{agoText(node.last_seen)}</dd></>}
          {!node.linked && node.link_error && <><dt className="type-callout text-muted-foreground">{t("devices:node.linkError")}</dt><dd className="type-callout break-all text-destructive" data-node-link-error>{agoText(node.link_error.at)} · {node.link_error.error}</dd></>}
        </dl>
      </DCard>
      {node.max_sessions != null && <Panel k="node-sessions" icon={<SquareTerminalIcon />} title={t("shell:devices.limit")} sub={t("shell:devices.limitSub", { count: node.max_sessions })}
        right={<RoomMeter running={node.running} max={node.max_sessions} waiting={node.waiting} />}>
        <div className="py-1.5"><SessionLimit max={node.max_sessions} editable={editable} save={(n) => api.nodeLimit(node.node_id, n).then(reloadNodes)} /></div>
      </Panel>}
      <NodeUpgradePanel node={node} reload={reloadNodes} />
      <NodeTags node={node.node_id} />
      {node.state !== "revoked" && <NodeContext node={node.node_id} value={node.owner_context ?? null} />}
      <VitalsSection node={node.node_id} />
      {editable && <DCard data-node-unpair><DRow title={t("devices:node.revoke")} sub={t("devices:node.revokeSub")}><Button size="sm" variant="ghost" className="text-destructive" onClick={() => setRevoking(true)}>{t("devices:node.revoke")}</Button></DRow></DCard>}
    </div>
    {renaming && <Rename v={v} onClose={() => setRenaming(false)} onDone={reloadNodes} />}
    {revoking && <Dialog open onOpenChange={(x) => !x && setRevoking(false)}><DialogContent className="sm:max-w-md" data-node-revoke-dialog>
      <DialogHeader><DialogTitle>{t("devices:node.revokeTitle", { name: node.name })}</DialogTitle><DialogDescription>{t("devices:node.revokeSub")}</DialogDescription></DialogHeader>
      <DialogFooter><Button variant="outline" onClick={() => setRevoking(false)}>{t("common:action.cancel")}</Button>
        <Button variant="destructive" disabled={busy} onClick={() => void act(() => api.revokeNode(node.node_id), t("devices:node.revoked", { name: node.name })).then(() => { setRevoking(false); go(BACK) })} data-node-revoke-confirm>{t("devices:node.revoke")}</Button></DialogFooter>
    </DialogContent></Dialog>}
  </Page></div>
}

function Rename({ v, onClose, onDone }: { v: NodeView; onClose: () => void; onDone: () => void }) {
  const t = useT()
  const [name, setName] = useState(v.name), [busy, setBusy] = useState(false)
  const save = async () => {
    setBusy(true)
    try { await api.renameNode(v.id, name.trim()); toast(t("devices:node.renamed", { name: name.trim() })); onClose(); onDone() } catch (e) { toast((e as Error).message); setBusy(false) }
  }
  return <Dialog open onOpenChange={(x) => !x && onClose()}><DialogContent className="sm:max-w-sm" data-node-rename-dialog>
    <DialogHeader><DialogTitle>{t("devices:node.renameTitle")}</DialogTitle></DialogHeader>
    <form onSubmit={(e) => { e.preventDefault(); void save() }} className="flex flex-col gap-4">
      <Input autoFocus value={name} maxLength={64} onChange={(e) => setName(e.target.value)} aria-label={t("devices:node.renameTitle")} data-node-rename-input />
      <DialogFooter><Button type="button" variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button><Button type="submit" disabled={busy || !name.trim() || name.trim() === v.name}>{t("common:action.save")}</Button></DialogFooter>
    </form>
  </DialogContent></Dialog>
}

/** how full a device is: running against its limit, and what waits for room */
function RoomMeter({ running, max, waiting }: { running?: number; max: number; waiting?: number }) {
  const t = useT()
  if (running == null || waiting == null) return null
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
