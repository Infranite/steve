/* Add a device: one dialog for both kinds, opened from the Devices page, the Hub switcher, the map's "Add a computer"
 * card, the Clients page, ⌘K, #/hub/pair and the overview's "Confirm" line (`openSheet("addDevice", { kind, node, ask })`).
 * A stepper (affectUI Stepper Form):
 *   - a Node (a computer that runs sessions): how it reaches the Hub, then the command to run there with a pairing
 *     code, then, once it has claimed the code, its fingerprint checked and the Node confirmed or rejected; or, without
 *     Steve on it yet, an install over tailcat that pairs it on its own (no fingerprint to compare by hand);
 *   - a client (a phone or another browser): the QR code of a pairing link.
 *
 * The fingerprint is checked in full. Typing two groups the Hub picks at random was considered and is weaker: a Node's
 * key outlives its pairings, so whoever learned it once can make a key that matches any two groups of its fingerprint
 * with about 2^32 tries and pass one check in 28 (and more groups with more work), while the whole fingerprint (128
 * bits) is out of reach. So all eight groups are entered, pasting fills them at once, and each is checked as it is
 * typed. The Hub's copy is not shown beside the field: copying it into the field would check nothing. */
import { useEffect, useRef, useState, type ClipboardEvent } from "react"
import { CheckCircle2Icon, MonitorSmartphoneIcon, RefreshCwIcon, ServerIcon, ShieldAlertIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { Switch } from "@/components/ui/switch"
import { CodeBlock } from "@/components/ui/code-block"
import { OtpInput } from "@/components/affect/otp-input-clean"
import { StepperForm, StepperSuccess, type Step, type StepperLabels } from "@/components/affect/stepper-form-clean"
import { api, type RInstall, type RNode, type RPairing, type RTailcat } from "@/api"
import { PairContent } from "@/devices-client-pair"
import { closeSheet, go } from "@/ui"
import { st, toast } from "@/store"
import { useT } from "@/i18n"
import { fpCheck, fpGroups, fpSpread, FP_GROUP, hostLine, hostOf, linkRoute, nodeViews } from "@/device-model"
import { nodeHref } from "@/node-detail-model"
import { useHubAddress, useHubs } from "@/hub-ui"
import { AgentTrail } from "@/components/affect/agent-trail-clean"
import { Progress } from "@/components/ui/progress"
import { failureOf, installTrail, resumable, transferDetails } from "@/devices-install"
import { cn } from "cn"

type Kind = "node" | "client"
type Via = "direct" | "tailcat" | "install"
type V = { kind: Kind; via: Via }
type StepId = "kind" | "via" | "run" | "confirm" | "install" | "scan"
/** how often the dialog looks for the Node that claimed the code */
const POLL = 2000

/** the steps of a flow, by what it adds and how; only the instance owner confirms a Node (anyone signed in may pair one) */
export function stepsOf(kind: Kind, via: Via, owner = true): StepId[] {
  if (kind === "client") return ["kind", "scan"]
  return via === "install" ? ["kind", "via", "install"] : owner ? ["kind", "via", "run", "confirm"] : ["kind", "via", "run"]
}

/** m:ss until `at` */
const left = (at: number, now: number) => { const s = Math.max(0, Math.floor((at - now) / 1000)); return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}` }

export function AddDeviceDialog(payload: { kind?: Kind; node?: string; ask?: boolean }) {
  const t = useT(), { hubs } = useHubs()
  const hub = hubs?.find((h) => h.current)?.name ?? "Hub"
  const address = useHubAddress()
  /* what opened it (`payload`): a kind to start with, a Node to confirm, an address that asked for a client code (made on a click) */
  const [values, setValues] = useState<V>({ kind: payload.kind ?? "node", via: "direct" })
  const steps = stepsOf(values.kind, values.via, st.owner)
  const [step, setStep] = useState(() => (payload.node ? 3 : payload.kind ? 1 : 0))
  const id = steps[Math.min(step, steps.length - 1)]
  const [now, setNow] = useState(Date.now())
  const [pairing, setPairing] = useState<RPairing | null>(null), [pairError, setPairError] = useState("")
  const [nodes, setNodes] = useState<RNode[] | null>(null)
  /* the pending Nodes there were when the code was made, and the ones rejected here: neither is "the one that arrived" */
  const before = useRef<Set<string> | null>(null), rejected = useRef(new Set<string>())
  const [typed, setTyped] = useState<string[]>([])
  const [tailcat, setTailcat] = useState<RTailcat | null>(null)
  const [confirmed, setConfirmed] = useState<RNode | null>(null)
  const [installOp, setInstallOp] = useState<RInstall | null>(null)

  useEffect(() => { const timer = setInterval(() => setNow(Date.now()), 1000); return () => clearInterval(timer) }, [])
  useEffect(() => { void api.nodesTailcat().then(setTailcat, () => setTailcat(null)) }, [])

  /* a code once the command is shown, and a new one when it expired */
  const makeCode = async () => {
    setPairError("")
    try {
      const pending = (await api.nodes().catch(() => ({ nodes: [] as RNode[] }))).nodes.filter((n) => n.state === "pending").map((n) => n.node_id)
      before.current = new Set(pending)
      setPairing(await api.createPairing())
    } catch (e) { setPairError((e as Error).message) }
  }
  useEffect(() => { if (id === "run" && !pairing && !pairError) void makeCode() }, [id, pairing, pairError])
  /* the Nodes, while one is awaited or confirmed */
  useEffect(() => {
    if (values.kind !== "node" || !st.owner || (id !== "run" && id !== "confirm")) return
    let live = true
    const read = () => void api.nodes().then((r) => { if (live) setNodes(r.nodes) }, () => {})
    read()
    const timer = setInterval(read, POLL)
    return () => { live = false; clearInterval(timer) }
  }, [values.kind, id])

  const pendings = (nodes ?? []).filter((n) => n.state === "pending" && !n.local && !rejected.current.has(n.node_id))
  const pending = payload.node ? pendings.find((n) => n.node_id === payload.node) ?? null
    : pendings.find((n) => before.current && !before.current.has(n.node_id)) ?? (before.current ? null : pendings[0] ?? null)
  /* the Node that claimed the code moves the dialog on to its fingerprint */
  const advanced = useRef<string | null>(null)
  useEffect(() => {
    if (id !== "run" || !pending || advanced.current === pending.node_id) return
    advanced.current = pending.node_id  /* once: Back from its fingerprint stays on the command */
    setTyped([]); setStep(steps.indexOf("confirm"))
  }, [id, pending?.node_id])

  const check = pending ? fpCheck(typed, pending.fingerprint) : null
  const reject = async () => {
    if (!pending) return
    try {
      await api.revokeNode(pending.node_id)
      rejected.current.add(pending.node_id)
      toast(t("devices:add.rejected", { name: pending.name }))
      setTyped([]); setPairing(null)
      if (payload.node) closeSheet(); else setStep(steps.indexOf("run"))
    } catch (e) { toast((e as Error).message) }
  }

  const labels: Partial<StepperLabels> = {
    continue: t("devices:add.stepper.next"), back: t("common:action.back"), backTo: (s) => t("devices:add.stepper.backTo", { step: s }),
    stepOf: (n, total) => t("devices:add.stepper.stepOf", { n, total }), allDone: () => t("devices:add.stepper.done"), progress: t("devices:add.stepper.progress"),
    done: t("devices:add.stepper.srDone"), current: t("devices:add.stepper.srCurrent"), finished: t("devices:add.stepper.done"), failed: t("devices:add.stepper.failed"),
    check: t("devices:add.stepper.check"), successTitle: t("devices:add.stepper.done"), successDesc: "",
  }
  const choice = (on: boolean, pick: () => void, title: string, sub: string, data: Record<string, string>, disabled = false, Icon?: typeof ServerIcon) => (
    <button type="button" role="radio" aria-checked={on} disabled={disabled} onClick={pick} {...data}
      className={cn("flex w-full items-start gap-3 rounded-xl px-4 py-3 text-left transition-[background-color,box-shadow] duration-120 ease-(--ease-out) focus-visible:ring-[3px] focus-visible:ring-ring/50 focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-55",
        on ? "bg-blue/6 shadow-[inset_0_0_0_1.5px_var(--blue)]" : "bg-muted/50 hover:bg-accent")}>
      {Icon ? <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-card text-muted-foreground shadow-[var(--lift)]" aria-hidden><Icon className="size-4" strokeWidth={1.75} /></span>
        : <span className={cn("mt-1 size-4 shrink-0 rounded-full", on ? "shadow-[inset_0_0_0_5px_var(--blue)]" : "shadow-[inset_0_0_0_1.5px_var(--input)]")} aria-hidden />}
      <span className="min-w-0 flex-1"><span className="block type-body font-medium text-foreground">{title}</span><span className="block type-footnote text-pretty text-muted-foreground">{sub}</span></span>
    </button>
  )
  const tailcatReady = !!tailcat?.enabled && !!tailcat.hub_url
  const hubUrl = values.via === "tailcat" ? tailcat?.hub_url ?? "" : address
  const pin = values.via === "direct" && pairing?.pin ? ` --pin ${pairing.pin}` : ""

  const all: Record<StepId, Step<V>> = {
    kind: { id: "kind", title: t("devices:add.step.kind"), heading: t("devices:add.heading.kind"),
      render: ({ values: v, setValues: set }) => <div role="radiogroup" className="flex flex-col gap-2">
        {choice(v.kind === "node", () => { set({ kind: "node" }); setValues((x) => ({ ...x, kind: "node" })) }, t("devices:add.node"), t("devices:add.nodeSub"), { "data-add-kind": "node" }, false, ServerIcon)}
        {choice(v.kind === "client", () => { set({ kind: "client" }); setValues((x) => ({ ...x, kind: "client" })) }, t("devices:add.client"), t("devices:add.clientSub"), { "data-add-kind": "client" }, false, MonitorSmartphoneIcon)}
      </div> },
    via: { id: "via", title: t("devices:add.step.via"), heading: t("devices:add.heading.via"),
      render: ({ values: v, setValues: set }) => {
        const pick = (via: Via) => { set({ via }); setValues((x) => ({ ...x, via })) }
        const noTailcat = !!tailcat && !tailcat.available && !tailcat.enabled
        return <div role="radiogroup" className="flex flex-col gap-2">
          {choice(v.via === "direct", () => pick("direct"), t("devices:add.via.direct"), address ? t("devices:add.via.directSub", { url: address }) : t("devices:add.via.directSubBare"), { "data-add-via": "direct" })}
          {choice(v.via === "tailcat", () => pick("tailcat"), t("devices:add.via.tailcat"), noTailcat ? t("devices:add.via.tailcatMissing") : t("devices:add.via.tailcatSub"), { "data-add-via": "tailcat" }, noTailcat || !tailcat)}
          {choice(v.via === "install", () => pick("install"), t("devices:add.via.install"), noTailcat ? t("devices:add.via.tailcatMissing") : t("devices:add.via.installSub"), { "data-add-via": "install" }, noTailcat || !tailcat || !st.owner)}
        </div>
      } },
    run: { id: "run", title: t("devices:add.step.run"), heading: t("devices:add.heading.run"),
      validate: () => (pending || !st.owner ? null : { node: t("devices:add.waitingNeeded") }),
      render: () => <div className="flex flex-col gap-3" data-add-run={values.via}>
        {values.via === "tailcat" && !tailcatReady && <TailcatSwitch s={tailcat} onChange={setTailcat} />}
        {pairError && <p role="alert" className="type-callout text-destructive">{pairError}</p>}
        {(values.via === "direct" || tailcatReady) && <>
          <div className="flex flex-col gap-2" data-install-steve>
            <p className="type-callout text-foreground">{t("devices:add.installCli")}</p>
            <CodeBlock compact wrap code="brew install infranite/tap/steve" language="bash" data-install-steve-command />
            <p className="type-footnote text-pretty text-muted-foreground">{t("devices:add.installCliPlatforms")} <a href="https://brew.sh/" target="_blank" rel="noreferrer" className="text-blue hover:underline">{t("devices:add.installHomebrew")}</a></p>
          </div>
          <p className="type-callout text-foreground">{t("devices:add.run")}</p>
          <CodeBlock compact wrap code={`steve setup node --hub ${hubUrl}${pin}`} language="bash" data-setup-command />
          {pairing && (pairing.expires > now ? <div className="flex flex-col gap-1.5" data-pair-code>
            <span className="type-footnote text-muted-foreground">{t("devices:add.code")} · {t("devices:add.codeExpires", { time: left(pairing.expires, now) })}</span>
            <CodeBlock compact wrap code={pairing.code} data-pair-code-value />
          </div> : <div className="flex items-center gap-3 type-callout text-muted-foreground">{t("devices:add.codeExpired")}<Button type="button" size="xs" variant="outline" onClick={() => setPairing(null)}><RefreshCwIcon />{t("devices:add.newCode")}</Button></div>)}
          <details className="type-footnote text-muted-foreground">
            <summary className="cursor-pointer hover:text-foreground">{t("devices:add.installed")}</summary>
            <div className="mt-2"><CodeBlock compact wrap code={`steve-node pair ${hubUrl}${pin}`} language="bash" data-pair-command /></div>
          </details>
          {values.via === "tailcat" && <p className="type-footnote text-pretty text-muted-foreground">{t("devices:add.tailcatSecret")}</p>}
          <div className="flex items-start gap-3 rounded-xl bg-blue/8 px-4 py-3 type-callout" role="status" data-add-waiting>
            <Spinner className="mt-0.5 size-4 shrink-0 text-blue" />
            <div><div className="text-foreground">{t("devices:add.waiting")}</div><div className="type-footnote text-muted-foreground">{st.owner ? t("devices:add.waitingSub") : t("devices:add.waitingOwner")}</div></div>
          </div>
        </>}
      </div> },
    confirm: { id: "confirm", title: t("devices:add.step.confirm"), heading: t("devices:add.heading.confirm"),
      validate: () => (!pending ? { fp: t("devices:add.noPending") } : check?.all ? null : { fp: t("devices:add.fpIncomplete") }),
      render: () => !pending ? <p className="type-callout text-muted-foreground" data-add-no-pending>{nodes ? t("devices:add.noPending") : <Spinner className="size-4" />}</p>
        : <div className="flex flex-col gap-3" data-add-confirm={pending.node_id}>
          <Arrived n={pending} />
          <p className="type-callout text-pretty text-foreground">{t("devices:add.fpAsk")}</p>
          <FingerprintInput fp={pending.fingerprint} typed={typed} onChange={setTyped} />
          <p className={cn("type-footnote text-pretty", check && check.wrong > 0 ? "text-destructive" : "text-muted-foreground")} data-fp-summary={check?.all ? "match" : check?.wrong ? "wrong" : "open"}>
            {check?.all ? <span className="inline-flex items-center gap-1.5 text-success"><CheckCircle2Icon className="size-3.5" aria-hidden />{t("devices:add.fpMatch")}</span>
              : check && check.wrong > 0 ? t("devices:add.fpMismatch", { count: check.wrong }) : t("devices:add.fpWhy")}
          </p>
        </div> },
    install: { id: "install", title: t("devices:add.step.install"), heading: t("devices:add.heading.install"),
      validate: () => (installOp?.state === "done" ? null : { install: t("devices:add.installNeeded") }),
      render: () => <RemoteInstall op={installOp} onChange={setInstallOp} /> },
    scan: { id: "scan", title: t("devices:add.step.scan"), heading: t("devices:add.heading.scan"),
      render: () => <PairContent ask={payload.ask === true} /> },
  }
  const submitLabel = id === "confirm" ? t("devices:add.confirm") : t("devices:add.done")
  return (
    <Dialog open onOpenChange={(x) => !x && closeSheet()}>
      <DialogContent className="max-h-[90vh] gap-3 overflow-y-auto rounded-2xl pb-0 sm:max-w-[600px]" data-add-device data-add-step={id}>
        <DialogHeader><DialogTitle className="type-title">{t("devices:add.title")}</DialogTitle>
          <DialogDescription className="type-callout">{values.kind === "client" && step > 0 ? t("devices:add.subClient") : t("devices:add.sub", { hub })}</DialogDescription></DialogHeader>
        <StepperForm<V>
          className="steve-add-stepper [--ui-stpc-max-width:none] [--ui-stpc-label-tracking:0]"
          steps={steps.map((s) => all[s])}
          initialValues={values}
          step={step}
          onStepChange={setStep}
          labels={labels}
          submitLabel={submitLabel}
          submittingLabel={submitLabel}
          aside={id === "confirm" && pending && st.owner ? <Button type="button" size="sm" variant="ghost" className="text-destructive" onClick={() => void reject()} data-add-reject>{t("devices:add.reject")}</Button> : undefined}
          onComplete={async () => {
            if (id === "confirm" && pending) { await api.confirmNode(pending.node_id); setConfirmed(pending); return }
            if (id === "install" && installOp?.node_id) { closeSheet(); go(nodeHref(installOp.node_id)); return }
            closeSheet()
          }}
          success={() => <StepperSuccess title={t("devices:add.confirmed", { name: confirmed?.name ?? "" })} description={t("devices:add.confirmedSub")}>
            <div className="flex flex-wrap justify-center gap-2 pt-2">
              {confirmed && <Button size="sm" variant="outline" onClick={() => { closeSheet(); go(nodeHref(confirmed.node_id)) }} data-add-open-node>{t("devices:add.openNode")}</Button>}
              <Button size="sm" onClick={() => closeSheet()} data-add-done>{t("devices:add.done")}</Button>
            </div>
          </StepperSuccess>}
        />
      </DialogContent>
    </Dialog>
  )
}

/** who claimed the code: its name, and where it came from and what it is when the Hub knows */
function Arrived({ n }: { n: RNode }) {
  const t = useT()
  const v = nodeViews([], [n])[0]
  const route = v ? linkRoute(v) : null
  const facts = [route ? t(`devices:route.${route}`) : "", n.link?.peer ?? "", v ? hostLine(hostOf(v)) : ""].filter(Boolean).join(" · ")
  return <div className="flex items-start gap-3 rounded-xl bg-attention/10 px-4 py-3 type-callout text-foreground" data-add-arrived>
    <ShieldAlertIcon className="mt-0.5 size-4 shrink-0 text-attention" aria-hidden />
    <span className="min-w-0 text-pretty">{facts ? t("devices:add.confirmWho", { name: n.name, facts }) : t("devices:add.confirmWhoBare", { name: n.name })}</span>
  </div>
}

/** The fingerprint in its eight groups (affectUI OTP Input, one per group): pasting anywhere fills every group from
 *  there (a whole fingerprint from the first), a full group moves on to the next, and each is checked once complete. */
function FingerprintInput({ fp, typed, onChange }: { fp: string; typed: string[]; onChange: (x: string[]) => void }) {
  const t = useT()
  const want = fpGroups(fp), { groups } = fpCheck(typed, fp)
  const box = useRef<HTMLDivElement>(null)
  const focus = (i: number) => box.current?.querySelector<HTMLInputElement>(`[data-fp-group="${i}"] input`)?.focus()
  const paste = (e: ClipboardEvent<HTMLDivElement>) => {
    const text = e.clipboardData.getData("text")
    if (text.replace(/[^0-9a-f]/gi, "").length <= FP_GROUP) return
    e.preventDefault(); e.stopPropagation()
    const at = Number((e.target as HTMLElement).closest<HTMLElement>("[data-fp-group]")?.dataset.fpGroup ?? 0)
    const next = fpSpread(typed, text, at, want.length)
    onChange(next)
    const open = fpCheck(next, fp).groups.findIndex((g) => g !== "match")
    if (open >= 0) requestAnimationFrame(() => focus(open))
  }
  return <div ref={box} className="grid grid-cols-2 gap-x-4 gap-y-2 sm:grid-cols-4 [--ui-otpc-box:34px] [--ui-otpc-gap:4px]" onPasteCapture={paste} data-fp-input>
    {want.map((g, i) => <div key={i} className="flex min-w-0 flex-col gap-0.5" data-fp-group={i} data-fp-state={groups[i]}>
      <span className="type-footnote tabular-nums text-tertiary">{i + 1}</span>
      <OtpInput length={g.length} numeric={false} value={typed[i] ?? ""} clearOnError={false} label={t("devices:add.fpGroup", { n: i + 1 })}
        status={groups[i] === "match" ? "success" : groups[i] === "wrong" ? "error" : "idle"} successLabel={g} errorMessage={t("devices:add.fpGroupWrong")}
        onChange={(code) => { const next = Array.from({ length: want.length }, (_, k) => (k === i ? code.toLowerCase() : typed[k] ?? "")); onChange(next); if (code.length === g.length && i + 1 < want.length) requestAnimationFrame(() => focus(i + 1)) }} />
    </div>)}
  </div>
}

/** tailcat for this pairing: on with a switch for the instance owner, a word to ask for it otherwise */
function TailcatSwitch({ s, onChange }: { s: RTailcat | null; onChange: (s: RTailcat) => void }) {
  const t = useT()
  const [busy, setBusy] = useState(false), [error, setError] = useState("")
  if (!s) return <Spinner className="size-4" />
  const turnOn = async () => {
    setBusy(true); setError("")
    try { onChange(await api.setNodesTailcat(true)) } catch (e) { setError((e as Error).message) } finally { setBusy(false) }
  }
  return <div className="flex flex-col gap-2 rounded-xl bg-muted/60 px-4 py-3 type-callout" data-tailcat>
    {s.enabled && !s.running ? <p className="text-muted-foreground">{s.error ? <span className="text-destructive">{s.error}</span> : t("devices:add.tailcatStarting")}</p>
      : st.owner ? <label className="flex items-center justify-between gap-3"><span>{t("devices:add.tailcatEnable")}</span><Switch checked={s.enabled} disabled={busy} onCheckedChange={() => void turnOn()} data-tailcat-switch /></label>
      : <p className="text-muted-foreground">{t("devices:add.tailcatNeedsOwner")}</p>}
    {error && <p role="alert" className="text-destructive">{error}</p>}
  </div>
}

/* A remote install over tailcat (A6b): a line to run on the target machine, its address pasted back, the rest done by
 * the Hub (download and verify, install, pair, start). A Node installed so needs no fingerprint compared by hand.
 * Each step shows how far it is (bytes and speed while fetching and sending, the setup's phase on the target) and how
 * long it took, all kept by the Hub: the dialog opened again picks up the install that runs (or the last one that
 * failed, to retry it). A failure says at which step and why. */
function RemoteInstall({ op, onChange }: { op: RInstall | null; onChange: (op: RInstall) => void }) {
  const t = useT()
  const [address, setAddress] = useState("")
  const [joined, setJoined] = useState(false)
  const [busy, setBusy] = useState(false), [error, setError] = useState("")
  /* when the shown copy came from the Hub, on this browser's clock: a running step's clock counts from it */
  const [receivedAt, setReceivedAt] = useState(() => Date.now())
  const show = (v: RInstall) => { setReceivedAt(Date.now()); if (v.address) setAddress((a) => a || v.address || ""); onChange(v) }
  const act = async (f: () => Promise<void>) => {
    setBusy(true); setError("")
    try { await f() } catch (e) { setError((e as Error).message) } finally { setBusy(false) }
  }
  useEffect(() => {
    if (op) return
    let live = true
    setBusy(true); setError("")
    void (async () => {
      try {
        const saved = resumable((await api.installs()).installs)
        // A closed step (or StrictMode's discarded effect) must not create another waiting install.
        if (!live) return
        const next = saved ?? await api.createInstall()
        if (live) show(next)
      } catch (e) { if (live) setError((e as Error).message) }
      finally { if (live) setBusy(false) }
    })()
    return () => { live = false }
  }, [])
  useEffect(() => {
    if (op?.state !== "running") return
    let live = true
    const timer = setInterval(() => { void api.install(op.id).then((v) => { if (live) show(v) }).catch(() => {}) }, 1000)
    return () => { live = false; clearInterval(timer) }
  }, [op?.id, op?.state])
  if (!op) return error ? <p role="alert" className="type-callout text-destructive">{error}</p> : <Spinner className="size-4" />
  /* the trail's keys are built from the step and phase names */
  const tk = (key: string, o?: Record<string, unknown>) => String(t(key as never, o as never))
  const trail = installTrail(op, receivedAt, tk)
  const current = trail.find((s) => s.state === "running")
  const transfer = transferDetails(op, tk)
  const failure = failureOf(op, tk)
  const run = () => void act(async () => {
    const v = await api.runInstall(op.id, address)
    setJoined(v.id !== op.id)
    show(v)
  })
  const cancel = () => void act(async () => {
    await api.cancelInstall(op.id)
    closeSheet()
  })
  return (
    <div className="flex flex-col gap-3 type-callout" data-remote-install>
      {joined && op.state === "running" && <p className="text-muted-foreground" data-install-joined>{t("shell:devices.install.joined")}</p>}
      {op.state !== "waiting" && <div className="flex flex-col gap-2" data-install-steps>
        <AgentTrail steps={trail} labels={{ title: t("shell:devices.install.progress"), running: t("shell:devices.install.working"), failed: t("shell:devices.install.failed"), done: t("shell:devices.install.finished"), pending: t("shell:devices.install.pending") }} />
        {transfer && <div className="flex min-w-0 flex-col gap-1 text-muted-foreground" data-install-transfer>
          <p>{transfer.route}</p>
          <div className="flex flex-wrap gap-x-4 gap-y-1 tabular-nums">
            {[transfer.bytes, transfer.rate, transfer.eta, transfer.transferTime, transfer.unpackTime]
              .map((fact, i) => fact && <span key={i}>{fact}</span>)}
          </div>
        </div>}
        {current?.percent !== undefined && <Progress value={current.percent} aria-label={current.title} data-install-percent />}
      </div>}
      {failure && <div role="alert" className="flex flex-col gap-1 text-destructive" data-install-failed>
        <p className="font-medium">{t("shell:devices.install.failedAt", { step: failure.step, elapsed: failure.elapsed })}</p>
        <p className="whitespace-pre-wrap">{failure.error}</p>
      </div>}
      {(op.state === "waiting" || op.state === "failed") && <>
        <p className="text-foreground">{t("shell:devices.install.step1")}</p>
        <CodeBlock compact wrap code={op.command} language="bash" data-install-command />
        <label className="text-foreground" htmlFor="install-address">{t("shell:devices.install.step2")}</label>
        <div className="flex gap-2">
          <Input id="install-address" className="font-mono" value={address} disabled={busy} onChange={(e) => setAddress(e.target.value)} placeholder="tc…" autoComplete="off" spellCheck={false} data-install-address />
          <Button type="button" disabled={busy || !address.trim()} onClick={run} data-install-run>{op.state === "failed" ? t("shell:devices.install.retry") : t("shell:devices.install.run")}</Button>
        </div>
        <div className="flex flex-col items-start gap-1">
          <Button type="button" variant="outline" size="sm" disabled={busy} onClick={cancel} data-install-cancel>{t("shell:devices.install.cancel")}</Button>
          <p className="type-footnote text-pretty text-muted-foreground">{t("shell:devices.install.cancelHint")}</p>
        </div>
      </>}
      {op.state === "done" && <p className="inline-flex items-center gap-1.5 text-success" data-install-done><CheckCircle2Icon className="size-4" aria-hidden />{t("shell:devices.install.done", { id: op.node_id ?? "" })}</p>}
      {error && <p role="alert" className="text-destructive">{error}</p>}
    </div>
  )
}
