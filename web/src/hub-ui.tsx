/* Hubs and pairing, the same in a browser and inside the apps: the switcher in the sidebar, the list with its add
 * field (on the no-Hub, signed-out and cannot-connect screens, in a dialog, and in 设置 · 访问与客户端), the answer
 * to a pairing link, and the offer of the Hubs another page sent along. The list itself is `hubDirectory()`. */
import { useCallback, useEffect, useMemo, useState } from "react"
import { CheckIcon, ChevronRightIcon, ChevronsUpDownIcon, KeyRoundIcon, PlusIcon, QrCodeIcon, ServerIcon, ServerOffIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { Input } from "@/components/ui/input"
import { SidebarMenuButton, SidebarMenuItem } from "@/components/ui/sidebar"
import { Spinner } from "@/components/ui/spinner"
import { cn } from "cn"
import { answerOffer, appPairLink, claimPair, currentHub, dropPairRequest, hubDirectory, HubError, hubsOffer, nativeBridge, onArrival, pairAddresses, pairIsHere, pairPlan, pairRequest, type HubEntry } from "@/hub"
import { t as tr, useT } from "@/i18n"
import { st, toast } from "@/store"
import { closeSheet, go, Group, openSheet, Row, SectionLabel } from "@/ui"

/** why a Hub could not be used, in the page's words */
export const hubErrorText = (e: unknown): string =>
  e instanceof HubError
    ? e.code === "api-mismatch" ? tr("shell:hub.apiMismatch", { version: e.version ?? "" }) : e.code === "bad-address" ? tr("shell:hub.badAddress") : e.code === "no-answer" ? tr("shell:hub.unreachable") : tr("shell:hub.notSteve")
    : (e as Error)?.message || String(e)

const hostOf = (u: string) => { try { return new URL(u).host } catch { return u } }

/** re-renders when a pairing or an offer of Hubs arrives or is answered */
function useArrival() {
  const [, bump] = useState(0)
  useEffect(() => onArrival(() => bump((n) => n + 1)), [])
}

/** the Hubs of where this page runs; `hubs` is null until the list is read */
export function useHubs() {
  const dir = useMemo(hubDirectory, [])
  const [hubs, setHubs] = useState<HubEntry[] | null>(null)
  const [error, setError] = useState("")
  const reload = useCallback(() => dir.list().then((xs) => { setHubs(xs); setError("") }, (e) => { setHubs([]); setError(hubErrorText(e)) }), [dir])
  useEffect(() => { void reload(); return onArrival(() => void reload()) }, [reload])
  return { dir, hubs, error, reload }
}

/** The address of the Hub in use, as another machine would type it. Inside a shell the page's own origin is the
 * shell's gateway on this machine, so the address is the shell's to tell; "" until it has. */
export function useHubAddress(): string {
  const { hubs } = useHubs()
  return hubs?.find((h) => h.current)?.address ?? (nativeBridge() ? "" : currentHub())
}

/** what is said under a Hub's name: its address when the name is not it, version, route, and whether it is signed in */
function hubSub(h: HubEntry): string {
  const route = h.route && ["loopback", "lan", "tailnet", "proxy", "public"].includes(h.route) ? tr(`pages:access.route.${h.route as "lan"}`) : h.route
  return [h.name !== hostOf(h.address) && h.name !== h.address ? hostOf(h.address) : "", h.version ?? "", route ?? "", h.signedIn === false ? tr("shell:hub.signedOut") : ""].filter(Boolean).join(" · ")
}

/** Said above every page while the live stream is broken: which Hub, that the page keeps trying, and the way to another. */
export function HubLostBanner() {
  const t = useT(), { dir, hubs } = useHubs()
  if (!st.down) return null
  const name = hubs?.find((h) => h.current)?.name ?? (dir.native ? "Hub" : hostOf(currentHub()))
  return (
    <div role="status" className="flex shrink-0 items-center gap-2 border-b bg-attention/10 px-4 py-2 type-footnote text-foreground/80 sm:px-6" data-hub-lost>
      <ServerOffIcon className="size-3.5 shrink-0 text-attention" aria-hidden />
      <span className="min-w-0 flex-1">{t("shell:hub.lost", { name })}</span>
      <Button size="xs" variant="ghost" onClick={() => openSheet("hubs")} data-hub-lost-switch>{t("shell:hub.other")}</Button>
    </div>
  )
}

/** the sidebar's Hub switcher, under the workspace switcher */
export function HubSwitcher() {
  const t = useT(), { dir, hubs } = useHubs()
  const cur = hubs?.find((h) => h.current)
  const name = cur?.name ?? (dir.native ? "Hub" : hostOf(currentHub()))
  const to = (h: HubEntry) => { if (!h.current) dir.switchTo(h).catch((e) => toast(hubErrorText(e))) }
  return (
    <SidebarMenuItem>
      <DropdownMenu>
        <DropdownMenuTrigger render={<SidebarMenuButton tooltip={t("shell:hub.tooltip", { name })} className="text-muted-foreground" data-hub-switch />}>
          <ServerIcon className="size-4! shrink-0" strokeWidth={1.5} />
          <span className="min-w-0 flex-1 truncate text-foreground/85">{name}</span><ChevronsUpDownIcon className="size-3.5! opacity-60" />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="min-w-60">
          {/* a label must sit inside a group: outside one, Base UI throws and blanks the page */}
          <DropdownMenuGroup>
            <DropdownMenuLabel className="type-footnote text-muted-foreground">Hub</DropdownMenuLabel>
            {(hubs ?? []).map((h) => (
              <DropdownMenuItem key={h.id} onClick={() => to(h)} data-hub-opt={h.address}>
                <ServerIcon className="size-4 text-muted-foreground" strokeWidth={1.5} />
                <span className="min-w-0 flex-1"><span className="block truncate">{h.name}</span>{hubSub(h) && <span className="block truncate type-footnote text-muted-foreground">{hubSub(h)}</span>}</span>
                {h.current && <CheckIcon className="size-4 text-blue" />}
              </DropdownMenuItem>))}
          </DropdownMenuGroup>
          <DropdownMenuSeparator />
          <DropdownMenuItem onClick={() => openSheet("hubs", { add: true })} data-hub-add><PlusIcon />{t("shell:hub.add")}</DropdownMenuItem>
          <DropdownMenuItem onClick={() => openSheet("pairClient")} data-hub-pair><QrCodeIcon />{t("shell:hub.pairDevice")}</DropdownMenuItem>
          <DropdownMenuItem onClick={() => go("#/settings/access")}><KeyRoundIcon />{t("shell:hub.access")}</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </SidebarMenuItem>
  )
}

/** the same entries on 我的, where a phone has no sidebar */
export function HubMore() {
  const t = useT(), { hubs } = useHubs()
  const cur = hubs?.find((h) => h.current)
  /* the tile the other rows of 我的 have (views.tsx `Ico`) */
  const ico = (I: typeof ServerIcon) => <span className="flex size-7 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground"><I className="size-4" strokeWidth={1.5} /></span>
  const more = <ChevronRightIcon className="size-4 text-tertiary" />
  return (
    <section data-more-hub><SectionLabel>Hub</SectionLabel><Group>
      <Row icon={ico(ServerIcon)} title={cur?.name ?? t("shell:hub.rowHubs")} sub={cur ? [hubSub(cur), t("shell:hub.rowHubs")].filter(Boolean).join(" · ") : undefined} onClick={() => openSheet("hubs")} data-more-hubs>{more}</Row>
      <Row icon={ico(QrCodeIcon)} title={t("shell:hub.rowPair")} sub={t("shell:hub.rowPairSub")} onClick={() => openSheet("pairClient")} data-more-pair>{more}</Row>
      <Row icon={ico(KeyRoundIcon)} title={t("shell:hub.rowAccess")} sub={t("shell:hub.rowAccessSub")} onClick={() => go("#/settings/access")} data-more-access>{more}</Row>
    </Group></section>
  )
}

/** The list of Hubs and the field that adds one (an address, or a pairing link). `manage`: with the name field and
 * "移除". `down`: the Hub in use did not answer, so it is offered again like the others, and a page that no Hub
 * served does not list its own address. */
export function HubChooser({ manage = false, down = false, focus = false, className }: { manage?: boolean; down?: boolean; focus?: boolean; className?: string }) {
  const t = useT(), { dir, hubs, error, reload } = useHubs()
  const [busy, setBusy] = useState("")
  /* what went wrong, and where it is said: under the list for one of its rows, under the field for an address typed */
  const [err, setErr] = useState("")
  const [errAt, setErrAt] = useState<"list" | "add">("add")
  const [input, setInput] = useState("")
  const [name, setName] = useState("")
  /* a switch that worked leaves this page (a reload, or the other Hub's page); what comes back here did not */
  const act = async (key: string, f: () => Promise<void>) => {
    if (busy) return
    setBusy(key); setErr("")
    try { await f() } catch (e) { setErr(hubErrorText(e)); setErrAt(key === "add" || key === "scan" ? "add" : "list") }
    setBusy(""); void reload()
  }
  const shown = (hubs ?? []).filter((h) => !(down && !dir.native && h.current && h.address === location.origin))
  return (
    <div className={cn("flex w-full flex-col gap-3 text-left", className)} data-hub-chooser>
      {hubs === null ? <Spinner className="size-4 self-center text-muted-foreground" /> : shown.length > 0 && (
        <div className="flex flex-col overflow-hidden rounded-xl border border-border" role="list" data-hub-list>
          {shown.map((h) => {
            const here = h.current && !down
            return (
              <div key={h.id} role="listitem" className="flex items-center gap-1 border-b border-border px-1 last:border-b-0" data-hub-row={h.address}>
                <button type="button" disabled={here || !!busy} onClick={() => void act(h.id, () => dir.switchTo(h))}
                  className={cn("flex min-w-0 flex-1 items-center gap-2.5 rounded-lg px-2 py-2 text-left", !here && "cursor-pointer hover:bg-accent")} data-hub-go={h.address}>
                  <ServerIcon className="size-4 shrink-0 text-muted-foreground" strokeWidth={1.5} />
                  <span className="min-w-0 flex-1">
                    <span className="block truncate type-body">{h.name}</span>
                    {(hubSub(h) || (h.current && down)) && <span className="block truncate type-footnote text-muted-foreground">{[h.current && down ? t("shell:hub.noAnswer") : "", hubSub(h)].filter(Boolean).join(" · ")}</span>}
                  </span>
                  {busy === h.id ? <Spinner className="size-4 text-muted-foreground" /> : here ? <CheckIcon className="size-4 text-blue" aria-label={t("shell:hub.current")} /> : null}
                </button>
                {manage && h.removable && <Button size="xs" variant="ghost" disabled={!!busy} onClick={() => void act("", () => dir.forget(h))} data-hub-forget={h.address}>{t("shell:hub.forget")}</Button>}
              </div>
            )
          })}
        </div>
      )}
      {error && <div className="type-footnote text-destructive" role="alert">{error}</div>}
      {err && errAt === "list" && <div className="-mt-1 type-footnote text-destructive" role="alert" data-hub-error>{err}</div>}
      <form onSubmit={(e) => { e.preventDefault(); if (input.trim()) void act("add", () => dir.add(input, name)) }} className="flex w-full flex-col gap-2" data-add-hub>
        <Input value={input} onChange={(e) => setInput(e.target.value)} placeholder={t("shell:hub.addPlaceholder")} autoFocus={focus} inputMode="url" autoCapitalize="off" autoCorrect="off" spellCheck={false} data-add-hub-url />
        {manage && <Input value={name} onChange={(e) => setName(e.target.value)} placeholder={t("shell:hub.namePlaceholder")} data-add-hub-name />}
        {err && errAt === "add" && <div className="type-footnote text-destructive" role="alert" data-hub-error>{err}</div>}
        <div className="flex gap-2">
          <Button type="submit" className="flex-1" disabled={!!busy || !input.trim()} data-add-hub-submit>{busy === "add" ? <Spinner className="size-4" /> : t("shell:hub.connect")}</Button>
          {dir.scan && <Button type="button" variant="outline" disabled={!!busy} onClick={() => void act("scan", dir.scan!)} data-hub-scan><QrCodeIcon className="size-4" />{t("shell:hub.scan")}</Button>}
        </div>
        <div className="type-footnote text-muted-foreground">{t("shell:hub.addHint")}</div>
      </form>
    </div>
  )
}

/** the shell's camera, where it has one: a scanned pairing link signs this app in */
export function ScanButton({ className }: { className?: string }) {
  const t = useT(), dir = useMemo(hubDirectory, [])
  if (!dir.scan) return null
  return <Button variant="outline" className={className} onClick={() => dir.scan!().catch((e) => toast(hubErrorText(e)))} data-hub-scan><QrCodeIcon className="size-4" />{t("shell:hub.scanSignIn")}</Button>
}

/** the Hub in use, as a line under a screen's title */
export function HubLine({ className }: { className?: string }) {
  const { hubs } = useHubs()
  const cur = hubs?.find((h) => h.current)
  const text = cur ? (cur.name === hostOf(cur.address) ? cur.name : `${cur.name} · ${hostOf(cur.address)}`) : nativeBridge() ? "" : hostOf(currentHub())
  return text ? <div className={cn("font-mono text-[12px] text-muted-foreground", className)} data-hub-line>{text}</div> : null
}

/** the list in a dialog: `openSheet("hubs")`, `#/hub/add` */
export function HubsDialog({ add = false }: { add?: boolean }) {
  const t = useT()
  return (
    <Dialog open onOpenChange={(o) => !o && closeSheet()}>
      <DialogContent className="sm:max-w-md" data-hubs-dialog>
        <DialogHeader>
          <DialogTitle>Hub</DialogTitle>
          <DialogDescription>{t("shell:hub.dialogSub")}</DialogDescription>
        </DialogHeader>
        <HubChooser manage focus={add} />
        <DialogFooter>
          <Button variant="outline" onClick={() => closeSheet()}>{t("common:action.close")}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/** The Hubs the page this one was reached from had in its list. Nothing is added before the person says so. */
export function HubsOffer({ className }: { className?: string }) {
  const t = useT()
  useArrival()
  const offer = hubsOffer()
  if (!offer.length) return null
  return (
    <div className={cn("flex w-full flex-col gap-2 rounded-xl border border-border p-3 text-left", className)} data-hubs-offer>
      <div className="type-callout">{t("shell:hub.offerTitle")}</div>
      <div className="type-footnote text-muted-foreground">{t("shell:hub.offerSub")}</div>
      <ul className="flex flex-col gap-0.5 font-mono text-[12px]">{offer.map((h) => <li key={h.url} className="truncate">{h.name ? `${h.name} · ${hostOf(h.url)}` : h.url}</li>)}</ul>
      <div className="flex gap-2">
        <Button size="sm" onClick={() => answerOffer(true)} data-hubs-offer-add>{t("shell:hub.offerAdd")}</Button>
        <Button size="sm" variant="ghost" onClick={() => answerOffer(false)} data-hubs-offer-skip>{t("shell:hub.offerSkip")}</Button>
      </div>
    </div>
  )
}

/** the same offer over a page that is in use */
export function HubsOfferDialog() {
  useArrival()
  if (!hubsOffer().length) return null
  return (
    <Dialog open onOpenChange={(o) => !o && answerOffer(false)}>
      <DialogContent className="sm:max-w-md" showCloseButton={false}>
        <DialogHeader className="sr-only"><DialogTitle>Hub</DialogTitle></DialogHeader>
        <HubsOffer className="border-0 p-0" />
      </DialogContent>
    </Dialog>
  )
}

/** whether a pairing waits for an answer (the screen for it comes before any other) */
export function usePairRequest() {
  useArrival()
  return pairRequest()
}

const APPLE = () => /iPhone|iPad|iPod|Macintosh/.test(navigator.userAgent)

/** A pairing link was opened in this page (a phone's camera read the QR code), or given to the add field: which Hub
 * it signs this browser in to, and the person's yes or no. Inside a shell the shell asks, and keeps the credential. */
export function PairClaim() {
  const t = useT()
  const req = pairRequest()
  const native = nativeBridge()
  const [busy, setBusy] = useState(false)
  const [err, setErr] = useState("")
  useEffect(() => {
    if (!native || !req?.link) return
    setBusy(true)
    native.call("pair.claim", { link: req.text }).then(() => dropPairRequest(), (e) => { setErr(hubErrorText(e)); setBusy(false) })
  }, [])
  if (!req) return null
  const link = req.link
  const plan = link && !native ? pairPlan(link) : null
  const addresses = link ? pairAddresses(link) : []
  const claim = async () => {
    if (!link) return
    setBusy(true); setErr("")
    try { await claimPair(link, st.webVersion); dropPairRequest(); location.reload() }
    catch (e) { setErr(hubErrorText(e)); setBusy(false) }
  }
  return (
    <div className="flex w-full max-w-sm flex-col items-center gap-4" data-pair-claim>
      <QrCodeIcon className="size-10 text-muted-foreground" strokeWidth={1.25} />
      <div>
        <div className="type-title">{link ? t("shell:hub.pairTitle", { name: link.name || hostOf(addresses[0] ?? "") || "Steve Hub" }) : t("shell:hub.pairBadTitle")}</div>
        {addresses.length > 0 && <div className="mt-1.5 flex flex-col gap-0.5 font-mono text-[12px] text-muted-foreground" data-pair-addresses>{addresses.map((u) => <span key={u}>{u}</span>)}</div>}
      </div>
      {!link || !addresses.length ? <p className="type-callout text-muted-foreground" data-pair-bad>{t("shell:hub.pairBad")}</p> : native ? (busy && <Spinner className="size-5 text-muted-foreground" />) : <>
        <p className="type-callout text-muted-foreground">{t("shell:hub.pairSub")}</p>
        {!pairIsHere(link) && <p className="rounded-lg bg-attention/10 px-3 py-2 type-footnote text-attention" data-pair-other>{t("shell:hub.pairOther")}</p>}
      </>}
      {err && <div className="type-footnote text-destructive" role="alert" data-pair-error>{err}</div>}
      <div className="flex w-full max-w-xs flex-col gap-2">
        {plan && addresses.length > 0 && (plan.call.length > 0
          ? <Button disabled={busy} onClick={() => void claim()} data-pair-accept>{busy ? <Spinner className="size-4" /> : t("shell:hub.pairHere")}</Button>
          : plan.open && <Button disabled={busy} onClick={() => { location.href = plan.open! }} data-pair-open>{t("shell:hub.pairOpenThere")}</Button>)}
        {link && !native && addresses.length > 0 && APPLE() && <Button variant="outline" disabled={busy} onClick={() => { location.href = appPairLink(link) }} data-pair-app>{t("shell:hub.pairInApp")}</Button>}
        <Button variant="ghost" disabled={busy && !native} onClick={() => dropPairRequest()} data-pair-cancel>{link && addresses.length ? t("common:action.cancel") : t("common:action.close")}</Button>
      </div>
    </div>
  )
}
