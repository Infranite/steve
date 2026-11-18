/* Devices · a Node (#/devices/nodes/<id>): the Hub's own Node and a paired one on one page. The head says what it is,
 * its one status and how it reaches the Hub, with "stop taking new work" and (for a paired Node) rename, unpair and
 * remove from its machine.
 * The left column is about now: its figures, its version, what it runs, its day. The right column is about its
 * settings: how it takes work, personal agent config, its harnesses, its identity, its project folders. An old link to
 * a device (#/devices/<device id>) lands here by the Node its row names. */
import { useState } from "react"
import { FolderIcon, MoreHorizontalIcon, PauseIcon, PlayIcon, PlusIcon, ShieldCheckIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { RuntimeSetupPanel } from "@/runtime-setup"
import { st, toast, refresh, useStore } from "@/store"
import { api } from "@/api"
import { go, Page, Empty, Loading } from "@/ui"
import { OwnerChip } from "@/people/owner-chip"
import { useT } from "@/i18n"
import { date } from "@/i18n/format"
import { homeTilde } from "@/resume"
import { agoText, findNode, fpGroups, hostLine, hostOf, linkRoute, nodeStatus, type NodeView } from "@/device-model"
import { useDevices, useNodeVitals } from "@/devices-data"
import { DCard, DRow, StatusBadge } from "@/devices-ui"
import { NodeFigures, NodeTrends } from "@/devices-vitals"
import { NodeVersion } from "@/devices-upgrade"
import { RunningCard } from "@/devices-work"
import { TakeWork, useWorkSettings } from "@/devices-config"
import { NodeContext } from "@/devices-context"
import { LeaveCard, RemoveDialog } from "@/devices-remove-dialog"

const BACK = "#/devices/nodes"

export function NodePage({ id }: { id: string }) {
  const t = useT(), { views, nodesRead, devicesError } = useDevices()
  const v = findNode(views, id)
  const back: [string, string] = [BACK, t("devices:node.back")]
  if (!v) return <Page title={t("devices:title")} back={back}>{nodesRead || devicesError ? <Empty title={t("devices:node.notFound")} /> : <Loading />}</Page>
  return <NodeDetail key={v.id} v={v} />
}

function NodeDetail({ v }: { v: NodeView }) {
  const t = useT(), { hubVersion, reloadNodes } = useDevices(), { value: vit } = useNodeVitals(v.id)
  useStore()
  const w = useWorkSettings(v)
  const [renaming, setRenaming] = useState(false), [revoking, setRevoking] = useState(false), [removing, setRemoving] = useState(false), [busy, setBusy] = useState(false)
  const status = nodeStatus(v, hubVersion), route = linkRoute(v), facts = hostLine(hostOf(v, vit?.latest))
  const remote = !!v.node && !v.local
  const pairable = remote && (v.state === "active" || v.state === "paused")
  const name = v.local ? t("devices:map.localNode") : v.name
  const via = v.local ? t("devices:node.onHub", { name: v.device?.name ?? "" }) : route ? t("devices:node.via", { route: t(`devices:route.${route}`) }) : ""
  const revoke = async () => {
    setBusy(true)
    try { await api.revokeNode(v.id); toast(t("devices:node.revoked", { name: v.name })); reloadNodes(); go(BACK) } catch (e) { toast((e as Error).message) } finally { setBusy(false); setRevoking(false) }
  }
  return <div className="device-page"><Page max={1040} back={[BACK, t("devices:node.back")]} toolbarTop
    title={<span className="inline-flex flex-wrap items-center gap-x-3 gap-y-1">{name}<StatusBadge status={status} /></span>}
    sub={<span className="inline-flex flex-wrap items-center gap-x-2 gap-y-0.5" data-node-sub>{[via, facts].filter(Boolean).map((p, i, all) => <span key={i} className="inline-flex items-center gap-2"><span>{p}</span>{i < all.length - 1 && <span className="text-tertiary" aria-hidden>·</span>}</span>)}</span>}
    toolbar={(w.editable || pairable) && <>
      {w.editable && v.state !== "pending" && <Button size="sm" variant="outline" disabled={w.busy} onClick={() => void w.setReceive(v.paused)} data-node-pause={v.paused ? "paused" : "active"}>
        {w.busy ? <Spinner className="size-3.5" /> : v.paused ? <PlayIcon /> : <PauseIcon />}{t(v.paused ? "devices:node.resume" : "devices:node.pause")}</Button>}
      {pairable && <DropdownMenu>
        <DropdownMenuTrigger render={<Button size="icon-sm" variant="outline" aria-label={t("devices:node.more")} data-node-more />}><MoreHorizontalIcon /></DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem onClick={() => setRenaming(true)} data-node-rename>{t("devices:node.rename")}</DropdownMenuItem>
          <DropdownMenuItem variant="destructive" onClick={() => setRevoking(true)} data-node-revoke>{t("devices:node.revoke")}</DropdownMenuItem>
          <DropdownMenuItem variant="destructive" onClick={() => setRemoving(true)} data-node-remove>{t("devices:remove.action")}</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>}
    </>}>
    <div className="grid items-start gap-4 lg:grid-cols-[minmax(0,1.6fr)_minmax(0,1fr)]" data-node-detail={v.id} data-node-page={v.id}>
      <div className="flex min-w-0 flex-col gap-4">
        <NodeFigures v={v} />
        <NodeVersion v={v} editable={st.owner && pairable} />
        <RunningCard id={v.device?.id ?? null} offline={!v.linked && !v.local} />
        <NodeTrends v={v} />
      </div>
      <div className="flex min-w-0 flex-col gap-4">
        <TakeWork v={v} w={w} />
        <NodeContext v={v} editable={v.local || pairable} />
        {v.local && <RuntimeSetupPanel>{v.device?.can_configure && !st.down && <AddHarness />}</RuntimeSetupPanel>}
        <Identity v={v} />
        <Projects v={v} />
        {pairable && <LeaveCard onUnpair={() => setRevoking(true)} onRemove={() => setRemoving(true)} />}
      </div>
    </div>
    {removing && <RemoveDialog v={v} onClose={() => setRemoving(false)} />}
    {renaming && <Rename v={v} onClose={() => setRenaming(false)} onDone={reloadNodes} />}
    {revoking && <Dialog open onOpenChange={(x) => !x && setRevoking(false)}><DialogContent className="sm:max-w-md" data-node-revoke-dialog>
      <DialogHeader><DialogTitle>{t("devices:node.revokeTitle", { name: v.name })}</DialogTitle><DialogDescription>{t("devices:node.revokeSub")}</DialogDescription></DialogHeader>
      <DialogFooter><Button variant="outline" onClick={() => setRevoking(false)}>{t("common:action.cancel")}</Button>
        <Button variant="destructive" disabled={busy} onClick={() => void revoke()} data-node-revoke-confirm>{t("devices:node.revoke")}</Button></DialogFooter>
    </DialogContent></Dialog>}
  </Page></div>
}

/** who and what it is: its id, the fingerprint checked when it was paired, its owner, when it last connected */
function Identity({ v }: { v: NodeView }) {
  const t = useT()
  const row = (k: string, val: React.ReactNode, data?: Record<string, string>) => <div className="contents" {...data}><dt className="type-callout text-muted-foreground">{k}</dt><dd className="min-w-0 type-callout [overflow-wrap:anywhere]">{val}</dd></div>
  return (
    <DCard icon={<ShieldCheckIcon />} title={t("devices:node.identity")} sub={t("devices:node.identitySub")} data-node-identity>
      <dl className="grid gap-x-6 gap-y-2.5 px-5 pb-5 sm:grid-cols-[7rem_minmax(0,1fr)]">
        {row(t("devices:node.nodeId"), <span className="type-code select-all">{v.legacy ? "—" : v.id}</span>)}
        {v.fingerprint && <><dt className="type-callout text-muted-foreground">{t("devices:node.fingerprint")}</dt><dd className="type-code select-all [overflow-wrap:anywhere]" data-node-fingerprint={v.fingerprint}>{fpGroups(v.fingerprint).join(" ") || v.fingerprint}</dd></>}
        {/* the Hub's own machine has a person responsible for it (the people module); a paired Node is its pairer's */}
        {v.local && v.device && <><dt className="type-callout text-muted-foreground">{t("devices:node.owner")}</dt><dd className="min-w-0"><OwnerChip objectType="device" objectId={v.device.id} bare /></dd></>}
        {v.lastSeen != null && row(t("devices:node.lastSeen"), <span className="tabular-nums" title={date(v.lastSeen, { dateStyle: "medium", timeStyle: "medium" })}>{v.linked ? t("devices:node.now") : agoText(v.lastSeen)}</span>)}
        {v.version && row(t("devices:card.version"), <span className="tabular-nums">{v.version}</span>)}
      </dl>
    </DCard>
  )
}

/** the project folders on this computer */
function Projects({ v }: { v: NodeView }) {
  const t = useT()
  const dirs = v.device ? st.projects.filter(p => p.device_id === v.device!.id) : []
  if (!dirs.length) return null
  return (
    <DCard icon={<FolderIcon />} title={t("devices:node.projects", { n: dirs.length })} data-node-projects>
      <div className="border-t border-border/70">{dirs.map(p => <DRow key={p.id} title={p.name} sub={<span className="break-all font-mono">{homeTilde(p.path)}</span>} onClick={() => go("#/projects/" + p.id)} />)}</div>
    </DCard>
  )
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
