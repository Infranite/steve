/* 设备与远程 Node 配对；身份与确认由所有者 API 校验。 */
import { useEffect, useState } from "react"
import { cn } from "cn"
import { ChevronRightIcon, PlusIcon, ServerIcon, CpuIcon, FolderIcon, PauseIcon, PlayIcon, RefreshCwIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Spinner } from "@/components/ui/spinner"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { st, toast, refresh, tm } from "@/store"
import { api, type RDevice, type RNode, type RPairing } from "@/api"
import { go, Page, Group, Row, SectionLabel, StatusDot, Meta, Empty, Status, closeSheet, openSheet } from "@/ui"
import { OwnerChip } from "@/people/owner-chip"
import { t, useT } from "@/i18n"
import { hubBase } from "@/hub"

const Ico = ({ d, size = 36 }: { d: RDevice; size?: number }) => <span className="relative flex shrink-0 items-center justify-center rounded-xl bg-muted text-muted-foreground" style={{ width: size, height: size }}><ServerIcon className="size-[45%]" strokeWidth={1.5} />
  <span className={cn("absolute -bottom-0.5 -right-0.5 size-2.5 rounded-full ring-2 ring-background", d.status === "暂停接活" ? "bg-attention" : "bg-success")} /></span>
const onDevice = (id: string) => Object.keys(st.S).filter((k) => st.S[k].device === id && !st.S[k].ro)
const stateText = (d: RDevice) => (d.status === "暂停接活" ? t("shell:devices.onlinePaused") : t("shell:devices.online"))

/** Node registration is separate from the legacy device inventory. Only the owner's Node API
 * establishes its visible identities and current transport links; Hub harnesses are not Node caps. */
function useRemoteNodes() {
  const [nodes, setNodes] = useState<RNode[] | null>(null)
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
        if (live) { setNodes([...new Map(r.nodes.filter(n => !n.local).map(n => [n.node_id, n])).values()]); setError("") }
      } catch (e) { if (live) setError((e as Error).message) }
      finally { if (live) { busy = false; setLoading(false); timer = setTimeout(() => void load(), 10_000) } }
    }
    const visible = () => { if (!document.hidden && !busy) { clearTimeout(timer); void load() } }
    void load()
    document.addEventListener("visibilitychange", visible)
    return () => { live = false; clearTimeout(timer); document.removeEventListener("visibilitychange", visible) }
  }, [revision])
  return { nodes, error, loading, reload: () => setRevision(x => x + 1) }
}

const NodeIco = ({ node, stale }: { node: RNode; stale: boolean }) => <span className="relative flex size-9 shrink-0 items-center justify-center rounded-xl bg-muted text-muted-foreground"><ServerIcon className="size-[45%]" strokeWidth={1.5} />
  <span className={cn("absolute -bottom-0.5 -right-0.5 size-2.5 rounded-full ring-2 ring-background", stale || node.state === "revoked" ? "bg-muted-foreground" : node.state === "pending" || node.state === "paused" ? "bg-attention" : node.linked ? "bg-success" : "bg-muted-foreground")} /></span>

export function DevicesView() {
  const t = useT()
  const { nodes, error, loading, reload } = useRemoteNodes()
  const [selected, setSelected] = useState<string | null>(null)
  const ids = new Set(nodes?.map(n => n.node_id))
  const ds = st.devices, hub = ds.find((d) => d.roles.includes("Hub")), rest = ds.filter((d) => d !== hub && !ids.has(d.id))
  const node = nodes?.find(n => n.node_id === selected)
  const connection = (n: RNode) => error ? t("shell:devices.connectionUnknown") : n.linked ? t("shell:devices.linked") : t("shell:devices.disconnected")
  const registration = (n: RNode) => error ? t("shell:devices.registrationPrevious", { state: t(`shell:devices.nodeState.${n.state}`) }) : t(`shell:devices.nodeState.${n.state}`)
  return (
    <>
    <Page title={t("shell:devices.title")} sub={t("shell:devices.sub")} max={760}
      actions={<Button size="sm" variant="outline" onClick={() => openSheet("pair")} data-pair><PlusIcon />{t("shell:devices.add")}</Button>}>
      <div className="flex flex-col gap-8">
        {hub && <section><SectionLabel>Hub</SectionLabel>
          <Group><Row icon={<Ico d={hub} />} onClick={() => go("#/devices/" + hub.id)} data-device-row={hub.name} title={<span className="inline-flex items-center gap-1.5">{hub.name}{hub.roles.map((r) => <Badge key={r} variant="outline" className="font-normal text-muted-foreground">{r}</Badge>)}</span>}
            sub={<Meta><span>{stateText(hub)}</span><span>{st.runtimes.filter((r) => r.ok).map((r) => r.name).join(t("shell:list.sep")) || t("shell:devices.noHarness")}</span></Meta>}><OwnerChip objectType="device" objectId={hub.id} bare /><ChevronRightIcon className="size-4 text-tertiary" /></Row></Group></section>}
        <section><SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" disabled={loading} onClick={reload} data-refresh-nodes>{loading ? <Spinner className="size-3" /> : <RefreshCwIcon />}{t("shell:devices.refreshNodes")}</Button>}>{nodes !== null && !error ? t("shell:devices.others", { count: rest.length + nodes.length }) : t("shell:devices.othersTitle")}</SectionLabel>
          {(rest.length > 0 || !!nodes?.length) && <Group>{rest.map((d) => <Row key={d.id} icon={<Ico d={d} />} title={d.name} sub={d.roles.join(" · ")} onClick={() => go("#/devices/" + d.id)}><OwnerChip objectType="device" objectId={d.id} bare /></Row>)}
            {nodes?.map(n => <Row key={n.node_id} icon={<NodeIco node={n} stale={!!error} />} data-node-row={n.node_id} onClick={() => setSelected(n.node_id)}
              title={<span className="inline-flex flex-wrap items-center gap-1.5">{n.name}<Badge variant="outline" className="font-normal text-muted-foreground">Node</Badge><Badge variant="outline" className="font-normal text-muted-foreground">{registration(n)}</Badge></span>}
              sub={<Meta><span>{connection(n)}</span><span className="font-mono break-all">{n.node_id}</span></Meta>}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}
          </Group>}
          {error && <p role="alert" className="px-1 py-3 type-callout text-destructive" data-node-error>{t("shell:devices.nodesUnavailable")} · {error}</p>}
          {nodes === null && !error && <p className="px-1 py-4 type-callout text-muted-foreground" role="status" data-nodes-loading>{t("shell:devices.loadingNodes")}</p>}
          {nodes !== null && !error && !rest.length && !nodes.length && <Empty title={t("shell:devices.noOthers")} sub={t("shell:devices.noOthersSub")} />}
        </section>
        <section><SectionLabel>{t("shell:devices.scheduling")}</SectionLabel>
          <Group><Row title={t("shell:devices.where")} sub={t("shell:devices.whereSub")} /></Group></section>
      </div>
    </Page>
    {selected && <Dialog open onOpenChange={open => !open && setSelected(null)}><DialogContent className="rounded-2xl sm:max-w-[560px]" data-node-detail>
      <DialogHeader><DialogTitle>{node?.name ?? t("shell:devices.remoteNode")}</DialogTitle><DialogDescription>{t("shell:devices.nodeDetailSub")}</DialogDescription></DialogHeader>
      {node ? <Group>
        <Row title={t("shell:devices.registrationState")} sub={registration(node)} />
        <Row title={t("shell:devices.connection")} sub={connection(node)} />
        <Row title={t("shell:devices.nodeId")} sub={<span className="font-mono break-all select-all">{node.node_id}</span>} />
        <Row title={t("shell:devices.fingerprint")} sub={<span className="font-mono break-all select-all">{node.fingerprint}</span>} />
        {node.last_seen != null && <Row title={t("shell:devices.lastConnected")} sub={tm(node.last_seen)} />}
      </Group> : <p role="status">{t("shell:devices.nodeNotVisible")}</p>}
      <DialogFooter><Button variant="outline" onClick={() => setSelected(null)}>{t("shell:devices.gotIt")}</Button></DialogFooter>
    </DialogContent></Dialog>}
    </>
  )
}

export function DeviceView({ id }: { id: string }) {
  const [busy, setBusy] = useState(false), t = useT()
  const d = st.devices.find((x) => x.id === id)
  if (!d) return <Page title={t("shell:devices.title")} back={["#/devices", t("shell:devices.title")]}><Empty title={t("shell:devices.notFound")} /></Page>
  const isHub = d.roles.includes("Hub"), ss = onDevice(d.id).filter((k) => st.S[k].st !== "ended"), dirs = st.projects.filter((p) => p.device_id === d.id)
  const recheck = async () => { setBusy(true); try { await api.devices(true); await refresh(); toast(t("shell:devices.rechecked")) } catch (e) { toast((e as Error).message) } finally { setBusy(false) } }
  return (
    <Page title={d.name} back={["#/devices", t("shell:devices.title")]} max={720} sub={<Meta><span>{d.roles.join(" · ")}</span><span>{stateText(d)}</span><span>{d.os}</span><span>{d.version}</span></Meta>}
      actions={<Button size="sm" variant="outline" onClick={() => void api.patchDevice(d.id, { status: d.status === "暂停接活" ? "正常" : "暂停接活" }).then(() => { toast(d.status === "暂停接活" ? t("shell:devices.resumed") : t("shell:devices.paused")) ; return refresh() })} data-pause-device>{d.status === "暂停接活" ? <><PlayIcon />{t("shell:devices.resume")}</> : <><PauseIcon />{t("shell:devices.pause")}</>}</Button>}>
      <div className="flex flex-col gap-8">
        {isHub && <section><SectionLabel>{t("shell:devices.hubNode")}</SectionLabel><Group>
          <Row title={t("shell:devices.storage")} sub={t("shell:devices.storageSub")} />
          <Row title={t("shell:devices.access")} sub={t("shell:devices.accessSub")} />
          <Row title={t("shell:devices.models")} sub={t("shell:devices.modelsSub")} />
        </Group></section>}
        <section><SectionLabel right={<span className="type-footnote font-normal">{t("shell:devices.maxSessions", { count: d.max_sessions })}</span>}>{t("shell:devices.sessionsHere", { count: ss.length })}</SectionLabel>
          {ss.length ? <Group>{ss.map((k) => { const s = st.S[k]; return <Row key={k} title={s.title} sub={s.item ? t("shell:devices.sessionSub", { item: s.item, n: s.n, up: s.up }) : t("shell:devices.noItem")} onClick={() => go("#/sessions/" + k)}><Status s={s.st} sess /></Row> })}</Group> : <Empty title={t("shell:devices.noSessions")} />}</section>
        <section><SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void recheck()}>{busy ? <Spinner className="size-3" /> : <RefreshCwIcon />}{t("shell:devices.recheck")}</Button>}>{t("shell:devices.runtimes")}</SectionLabel>
          {st.runtimes.length ? <Group>{st.runtimes.map((r) => <Row key={r.id} icon={<span className="flex size-8 items-center justify-center rounded-lg bg-muted text-muted-foreground"><CpuIcon className="size-4" strokeWidth={1.5} /></span>}
            title={<span className="inline-flex items-center gap-1.5">{r.name}<span className="type-footnote text-muted-foreground">{r.version}</span></span>} sub={<span>{r.note} · <span className="font-mono">{r.path.replace(/^\/Users\/[^/]+/, "~")}</span></span>}>
            <span className="inline-flex items-center gap-1.5 type-footnote text-muted-foreground"><StatusDot s={r.ok ? "done" : "wait"} />{r.ok ? t("shell:devices.available") : t("shell:devices.unavailable")}</span></Row>)}</Group> : <Empty title={t("shell:devices.noRuntimes")} />}</section>
        <section><SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => go("#/projects")}>{t("shell:devices.manageProjects")}</Button>}>{t("shell:devices.projectsHere", { count: dirs.length })}</SectionLabel>
          {dirs.length ? <Group>{dirs.map((p) => <Row key={p.id} icon={<FolderIcon className="size-4 text-muted-foreground" strokeWidth={1.5} />} title={p.name} sub={<span className="font-mono">{p.path.replace(/^\/Users\/[^/]+/, "~")}</span>} onClick={() => go("#/projects/" + p.id)}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Group>
            : <Empty title={t("shell:devices.noProjects")} />}</section>
        <section><SectionLabel>{t("shell:devices.connection")}</SectionLabel><Group><Row title={t("shell:devices.lastSeen")} sub={tm(d.seen)} /><Row title={t("shell:devices.owner")} sub={t("shell:devices.ownerSub")}><OwnerChip objectType="device" objectId={d.id} bare /></Row></Group></section>
      </div>
    </Page>
  )
}

export function PairDialog() {
  const t = useT()
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
          {/* the Hub's own address: this page may be served apart from it (A3) */}
          <p>{t("shell:devices.setupCommand")}</p><code className="block break-all select-all" data-setup-command>steve setup node --hub {hubBase() || window.location.origin}{pairing.pin ? ` --pin ${pairing.pin}` : ""}</code>
          <p>{t("shell:devices.pairCommand")}</p><code className="block break-all select-all" data-pair-command>steve-node pair {hubBase() || window.location.origin}{pairing.pin ? ` --pin ${pairing.pin}` : ""}</code>
        </div>}
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
