/* Signing another device in to this Hub: the QR code of a pairing link, and the link itself (A4). Opened from Add
 * device, the Clients page, ⌘K and `#/hub/pair`. */
import { useT } from "@/i18n"
import { useEffect, useMemo, useState } from "react"
import { CheckCircle2Icon, RefreshCwIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Spinner } from "@/components/ui/spinner"
import { access, type ClientPairing, type PairingStatus } from "@/access-api"
import { encodeQr, qrSvgPath } from "@/qr"
import { toast } from "@/store"
import { closeSheet, Pill } from "@/ui"
import { cn } from "cn"

function QrSvg({ text, size = 216 }: { text: string; size?: number }) {
  const rendered = useMemo(() => {
    try {
      return qrSvgPath(encodeQr(text), 2)
    } catch {
      return null
    }
  }, [text])
  if (!rendered) return null
  return (
    <div className="inline-flex rounded-xl bg-white p-3 shadow-[0_0_0_1px_rgb(16_24_40/0.06),0_2px_8px_-2px_rgb(16_24_40/0.1)]" data-pair-qr>
      <svg viewBox={rendered.viewBox} width={size} height={size} shapeRendering="crispEdges" role="img" aria-label="QR Code">
        <rect width="100%" height="100%" fill="#ffffff" />
        <path d={rendered.d} fill="#09090b" />
      </svg>
    </div>
  )
}

/** The pairing of another device: the QR code of a pairing link, and the link itself. A phone's camera opens the
 * link in a browser, the Steve app reads the same code. */
export function PairContent({ onClaimed, ask = false }: { onClaimed?: () => void; ask?: boolean }) {
  const t = useT()
  const [pair, setPair] = useState<ClientPairing | null>(null)
  /* the address the code is drawn for: a device that cannot reach the first is given another */
  const [at, setAt] = useState(0)
  const [status, setStatus] = useState<PairingStatus | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState("")

  const generate = async () => {
    setBusy(true)
    setError("")
    try {
      const p = await access.createPairing()
      setPair(p)
      setAt(0)
      setStatus({ id: p.id, status: "pending", expires: p.expires, client: null })
    } catch (e) {
      setError((e as Error).message)
    }
    setBusy(false)
  }

  /* `ask`: the dialog was opened by an address, and an address alone does not make a code that signs a device in */
  const [asked, setAsked] = useState(ask)
  useEffect(() => {
    if (!ask) void generate()
  }, [])

  useEffect(() => {
    if (!pair || (status && status.status !== "pending")) return
    let live = true
    const poll = async () => {
      try {
        const s = await access.pairingStatus(pair.id)
        if (!live) return
        setStatus(s)
        if (s.status === "claimed" && s.client != null) onClaimed?.()
      } catch {
        /* transient poll error */
      }
    }
    const id = setInterval(() => void poll(), 2000)
    return () => {
      live = false
      clearInterval(id)
    }
  }, [pair?.id, status?.status])

  if (error) {
    return (
      <div className="flex flex-col items-start gap-3">
        <p className="type-callout text-destructive">{error}</p>
        <Button size="sm" variant="outline" onClick={() => void generate()}>{t("pages:access.pairRefresh")}</Button>
      </div>
    )
  }

  if (asked && !pair && !busy) {
    return <div className="flex justify-center py-6"><Button onClick={() => { setAsked(false); void generate() }} data-pair-show>{t("pages:access.pairShow")}</Button></div>
  }

  if (!pair || busy) {
    return <div className="flex items-center justify-center py-8"><Spinner className="size-5 text-muted-foreground" /></div>
  }

  const link = pair.links[Math.min(at, pair.links.length - 1)]
  const curStatus = status?.status === "claimed" && status.client != null ? "claimed" : status?.status === "expired" ? "expired" : "pending"

  return (
    <div className="flex flex-col items-center gap-4 text-center" data-pair-card>
      {link && curStatus === "pending" && <QrSvg text={link.link} />}

      {!link && (
        <p className="max-w-sm rounded-xl bg-attention/12 px-4 py-3 type-callout text-foreground/90" data-pair-loopback>
          {t("pages:access.pairRouteLoopback")}
        </p>
      )}

      {link && (
        <div className="w-full max-w-sm rounded-xl bg-muted/60 p-1.5 text-left" data-pair-addresses>
          <div className="mb-1 px-2 pt-1 type-label text-tertiary">{t(pair.links.length > 1 ? "pages:access.pairAddressPick" : "pages:access.pairAddress")}</div>
          <div className="flex flex-col gap-0.5">
            {pair.links.map((l, n) => (
              <button key={l.url} type="button" onClick={() => setAt(n)} aria-pressed={l === link} disabled={pair.links.length < 2}
                className={cn("flex items-center justify-between gap-2 rounded-lg px-2 py-1.5 text-left type-code", pair.links.length > 1 && "cursor-pointer hover:bg-background/70", l === link && pair.links.length > 1 && "bg-card shadow-[var(--lift)]")} data-pair-address={l.url}>
                <span className="truncate">{l.url}</span>
                <Pill className="font-sans">{t(`pages:access.route.${l.kind}`)}</Pill>
              </button>
            ))}
          </div>
        </div>
      )}

      <div className="flex items-center gap-2 type-footnote" data-pair-status={curStatus}>
        {curStatus === "claimed" ? (
          <span className="inline-flex items-center gap-1.5 font-medium text-success">
            <CheckCircle2Icon className="size-4" />
            {t("pages:access.pairClaimed", { name: status?.client?.name ?? "" })}
          </span>
        ) : curStatus === "expired" ? (
          <span className="text-destructive">{t("pages:access.pairExpired")}</span>
        ) : (
          <span className="inline-flex items-center gap-2 text-muted-foreground">
            <Spinner className="size-3.5" />
            {t("pages:access.pairWaiting")}
          </span>
        )}
      </div>

      <div className="flex flex-wrap items-center justify-center gap-2">
        <Button size="sm" variant="outline" onClick={() => void generate()} data-pair-refresh>
          <RefreshCwIcon className="size-3.5" />
          {t("pages:access.pairRefresh")}
        </Button>
        {link && curStatus === "pending" && (
          <Button
            size="sm"
            variant="ghost"
            onClick={() => {
              void navigator.clipboard?.writeText(link.link)
              toast(t("pages:access.copied"))
            }}
            data-pair-copy
          >
            {t("pages:access.pairCopyUri")}
          </Button>
        )}
      </div>
    </div>
  )
}

/** the pairing in a dialog: `openSheet("pairClient")`, from Add device, the Clients page, ⌘K and `#/hub/pair`
 * (`ask`: opened by an address, so the code is made on a click) */
export function PairDeviceDialog({ ask }: { ask?: boolean }) {
  const t = useT()
  return (
    <Dialog open onOpenChange={(o) => !o && closeSheet()}>
      <DialogContent className="sm:max-w-md" data-pair-device-dialog>
        <DialogHeader>
          <DialogTitle>{t("pages:access.pairTitle")}</DialogTitle>
          <DialogDescription>{t("pages:access.pairSub")}</DialogDescription>
        </DialogHeader>
        <PairContent ask={ask} />
        <DialogFooter>
          <Button variant="outline" onClick={() => closeSheet()}>{t("pages:access.done")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
