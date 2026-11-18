import { useCallback, useEffect, useRef, useState } from "react"
import { DownloadIcon, RefreshCwIcon } from "lucide-react"
import { api } from "@/api"
import { IntegrationSettings, IntegrationSection } from "@/components/affect/integration-settings-clean"
import { Badge } from "@/components/affect/badge-clean"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { HarnessMark } from "@/components/harness-mark"
import { DCard } from "@/devices-ui"
import { useT } from "@/i18n"
import { monthDayTime } from "@/i18n/format"
import { st, useStore } from "@/store"
import { receiptStatus, updateTargets, type HarnessUpdates, type HarnessUpdate, type HarnessReceipt } from "@/harness-update-status"
import type { NodeView } from "@/device-model"

/** Cache by Node id only; every receipt and installed version comes from that target's API. */
const cache = new Map<string, HarnessUpdates>()

export function NodeHarnessUpdates({ v }: { v: NodeView }) {
  useStore()
  const t = useT(), [data, setData] = useState<HarnessUpdates | null>(() => cache.get(v.id) ?? null)
  const [busy, setBusy] = useState(false), [error, setError] = useState("")
  const [message, setMessage] = useState("")
  const [requesting, setRequesting] = useState<string | null>(null)
  const reading = useRef(false), sending = useRef(false), mounted = useRef(true)
  const reload = useCallback(async (fresh = false) => {
    if (reading.current) return
    reading.current = true
    setBusy(true)
    try {
      const result = await api.harnessUpdates(v.id, fresh)
      if (result.report && result.report.node !== v.id) throw new Error(t("devices:harnessUpdates.wrongNode"))
      cache.set(v.id, result)
      if (mounted.current) { setData(result); setError("") }
    } catch (e) { if (mounted.current) setError((e as Error).message) }
    finally { reading.current = false; if (mounted.current) setBusy(false) }
  }, [v.id, t])
  useEffect(() => {
    mounted.current = true
    if (st.owner && !v.legacy && v.state !== "revoked") void reload()
    return () => { mounted.current = false }
  }, [reload, v.legacy, v.state, st.owner])
  const active = data?.report?.checking || Object.values(data?.runs ?? {}).some(r => ["running", "unknown"].includes(receiptStatus(r) ?? ""))
  useEffect(() => {
    if (!st.owner || v.legacy || v.state === "revoked") return
    const timer = setInterval(() => { if (!document.hidden) void reload() }, active ? 1500 : 10_000)
    const visible = () => { if (!document.hidden) void reload() }
    document.addEventListener("visibilitychange", visible)
    return () => { clearInterval(timer); document.removeEventListener("visibilitychange", visible) }
  }, [reload, active, v.legacy, v.state, st.owner])
  if (v.legacy || v.state === "revoked") return null
  const upgrade = async (h: HarnessUpdate, retry = false) => {
    const versions = updateTargets(h)
    if (sending.current || !h.can_upgrade || !Object.keys(versions).length || !v.linked || !st.owner || data?.error) return
    sending.current = true
    setRequesting(h.harness); setMessage("")
    try {
      const result = await api.upgradeHarness(v.id, h.harness, "hup_" + crypto.randomUUID().replaceAll("-", ""), versions, retry)
      if (mounted.current) setData(prev => prev && ({ ...prev, runs: { ...prev.runs, [h.harness]: result.receipt } }))
    } catch (e) { if (mounted.current) setMessage((e as Error).message) }
    finally { sending.current = false; if (mounted.current) { setRequesting(null); void reload() } }
  }
  const harnesses = data?.report?.harnesses ?? []
  const shown = harnesses.filter(h => h.components.some(c => !["not_installed", "unsupported"].includes(c.state)) || h.operation || data?.runs[h.harness])
  const other = harnesses.filter(h => !shown.includes(h))
  const unavailable = !!data?.error || !!error
  return <DCard icon={<DownloadIcon />} title={t("devices:harnessUpdates.title")} sub={t("devices:harnessUpdates.sub")} data-harness-updates={v.id}>
    <div className="flex flex-col gap-3 px-3 pb-3">
      <div className="flex flex-wrap items-center justify-between gap-2 px-2">
        <span className="type-footnote text-muted-foreground" role="status">{data?.report?.checking ? t("devices:harnessUpdates.checking") : data?.at ? t("devices:harnessUpdates.observed", { at: monthDayTime(data.at) }) : ""}</span>
        <Button size="sm" variant="outline" disabled={!st.owner || !v.linked || busy || !!data?.report?.checking || !!requesting} onClick={() => void reload(true)} data-harness-update-check>
          {busy || data?.report?.checking ? <Spinner /> : <RefreshCwIcon />}{t("devices:harnessUpdates.check")}
        </Button>
      </div>
      {!st.owner && <p className="px-2 type-footnote text-muted-foreground">{t("devices:harnessUpdates.owner")}</p>}
      {(unavailable || message) && <div className="px-2 type-footnote text-attention" role="alert" data-harness-update-error>
        <p>{t(data?.code === "unsupported" ? "devices:harnessUpdates.oldNode" : "devices:harnessUpdates.unavailable")}</p>
        {data?.report && <p>{t("devices:harnessUpdates.stale")}</p>}
        <details className="mt-1"><summary className="cursor-pointer">{t("devices:harnessUpdates.details")}</summary><p className="break-words whitespace-pre-wrap">{message || error || data?.error}</p></details>
      </div>}
      {!harnesses.length && st.owner && !unavailable && <p className="px-2 type-footnote text-muted-foreground">{t(busy ? "devices:harnessUpdates.loading" : "devices:harnessUpdates.empty")}</p>}
      {shown.map(h => <UpdateCard key={h.harness} h={h} receipt={data?.runs[h.harness] ?? h.operation} requesting={requesting === h.harness}
        disabled={!st.owner || !v.linked || v.state !== "active" || unavailable || !!requesting}
        retry={!!data?.retryable_unknown?.includes(h.harness)} upgrade={retry => void upgrade(h, retry)} />)}
      {!!other.length && <details className="px-2 type-footnote text-muted-foreground" data-harness-update-other><summary className="cursor-pointer">{t("devices:harnessUpdates.other", { count: other.length })}</summary>
        <div className="mt-3 flex flex-col gap-3">{other.map(h => <UpdateCard key={h.harness} h={h} receipt={data?.runs[h.harness] ?? h.operation} disabled requesting={false} retry={false} upgrade={() => {}} />)}</div>
      </details>}
    </div>
  </DCard>
}

function UpdateCard({ h, receipt, requesting, disabled, retry, upgrade }: { h: HarnessUpdate; receipt?: HarnessReceipt | null; requesting: boolean; disabled: boolean; retry: boolean; upgrade: (retry: boolean) => void }) {
  const t = useT(), status = receiptStatus(receipt), available = h.components.some(c => c.state === "available")
  const detail = receipt?.state.state === "unknown" ? receipt.state.reason : receipt?.state.state === "finished" ? receipt.state.error?.safe_detail : null
  return <IntegrationSettings name={h.name} logo={<HarnessMark harness={h.harness} className="size-5" />} meta={available ? <Badge tone="warning">{t("devices:harnessUpdates.available")}</Badge> : undefined} data-harness-update={h.harness}>
    <IntegrationSection><ul className="flex flex-col gap-3 type-footnote">{h.components.map(c => <li key={c.id} data-harness-update-component={c.id}>
      <div className="flex flex-wrap items-center gap-2"><span className="font-medium">{c.name}</span><Badge tone={c.state === "available" || c.state === "check_failed" ? "warning" : "neutral"}>{t(`devices:harnessUpdates.state.${c.state}`)}</Badge></div>
      <p className="mt-1 font-mono text-muted-foreground">{c.installed ?? "—"}{c.latest && c.latest !== c.installed ? " → " + c.latest : ""}</p>
      {!c.managed && c.state !== "not_installed" && <p className="mt-1 text-muted-foreground">{t("devices:harnessUpdates.external")}</p>}
      {(c.reason || c.package || c.checked) && <details className="mt-1 text-muted-foreground"><summary className="cursor-pointer">{t("devices:harnessUpdates.details")}</summary>
        {c.package && <p className="break-all font-mono">{c.package}</p>}{c.checked && <p>{t("devices:harnessUpdates.checked", { at: monthDayTime(c.checked) })}</p>}{c.reason && <p className="break-words whitespace-pre-wrap">{c.reason}</p>}
      </details>}
    </li>)}</ul></IntegrationSection>
    {(Object.keys(updateTargets(h)).length > 0 || receipt || requesting) && <IntegrationSection>
      <div className="flex flex-wrap items-center gap-2">
        {Object.keys(updateTargets(h)).length > 0 && <Button size="sm" disabled={disabled || !h.can_upgrade || status === "running" || (status === "unknown" && !retry)} onClick={() => upgrade(status === "unknown")} data-harness-upgrade={h.harness}>
          {requesting || status === "running" ? <Spinner /> : <DownloadIcon />}{t(requesting ? "devices:harnessUpdates.requesting" : status === "running" ? "devices:harnessUpdates.running" : status === "unknown" && retry ? "devices:harnessUpdates.retry" : "devices:harnessUpdates.upgrade")}
        </Button>}
        {status && <span role="status" className={status === "failed" || status === "unknown" ? "type-footnote text-attention" : "type-footnote text-muted-foreground"} data-harness-upgrade-result={status}>{t(`devices:harnessUpdates.result.${status}`)}</span>}
      </div>
      {status === "unknown" && <p className="mt-2 type-footnote text-muted-foreground">{t(retry ? "devices:harnessUpdates.unknownRetry" : "devices:harnessUpdates.unknown")}</p>}
      {(h.reason || receipt) && <details className="mt-2 type-footnote text-muted-foreground"><summary className="cursor-pointer">{t("devices:harnessUpdates.details")}</summary>{h.reason && <p className="break-words whitespace-pre-wrap">{h.reason}</p>}{receipt && <p className="break-all font-mono" data-harness-upgrade-operation>{receipt.operation}</p>}{detail && <p className="break-words whitespace-pre-wrap">{detail}</p>}</details>}
    </IntegrationSection>}
  </IntegrationSettings>
}
