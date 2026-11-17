/* Add a device: one entry, from the Devices page, the Hub switcher, ⌘K and the Clients page, for both kinds. A Node
 * (a computer that runs sessions) is paired with a code and its fingerprint confirmed; a client (a phone or another
 * browser) signs in with a QR code. */
import { useEffect, useState } from "react"
import { MonitorSmartphoneIcon, RefreshCwIcon, ServerIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { api, type RNode, type RPairing } from "@/api"
import { closeSheet, openSheet } from "@/ui"
import { CODE, TailcatSection } from "@/devices-tailcat"
import { useT } from "@/i18n"
import { hm } from "@/i18n/format"
import { useHubAddress, useHubs } from "@/hub-ui"

/** the first step: which kind of device; the next is the Node pairing or the client's QR code */
export function AddDeviceDialog() {
  const t = useT(), { hubs } = useHubs()
  const hub = hubs?.find((h) => h.current)?.name ?? "Hub"
  const choice = (kind: "pair" | "pairClient", Icon: typeof ServerIcon, title: string, sub: string) => (
    <button type="button" onClick={() => openSheet(kind)} data-add-kind={kind === "pair" ? "node" : "client"}
      className="flex w-full items-start gap-3 rounded-xl bg-muted/50 px-4 py-3.5 text-left transition-colors duration-120 ease-(--ease-out) hover:bg-accent focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none">
      <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-card text-muted-foreground shadow-[var(--lift)]" aria-hidden><Icon className="size-4" strokeWidth={1.75} /></span>
      <span className="min-w-0 flex-1"><span className="block type-body font-medium">{title}</span><span className="block type-footnote text-muted-foreground">{sub}</span></span>
    </button>
  )
  return (
    <Dialog open onOpenChange={(x) => !x && closeSheet()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[480px]" data-add-device>
        <DialogHeader><DialogTitle className="type-title">{t("devices:add.title")}</DialogTitle><DialogDescription className="type-callout">{t("devices:add.sub", { hub })}</DialogDescription></DialogHeader>
        <div className="flex flex-col gap-2">
          {choice("pair", ServerIcon, t("devices:add.node"), t("devices:add.nodeSub"))}
          {choice("pairClient", MonitorSmartphoneIcon, t("devices:add.client"), t("devices:add.clientSub"))}
        </div>
      </DialogContent>
    </Dialog>
  )
}

/** a Node: a pairing code and the commands for the other computer, tailcat, and the pending Nodes to confirm */
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
