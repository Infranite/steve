import { useEffect, useState } from "react"
import { api, type RDevice } from "@/api"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Switch } from "@/components/ui/switch"
import { st, refresh } from "@/store"
import { useT } from "@/i18n"

/** The draft stays independent of live refreshes. A conflict never silently rebases it. */
export function DeviceConfiguration({ device, available = true }: { device: RDevice; available?: boolean }) {
  const t = useT(), [base, setBase] = useState(device), [receive, setReceive] = useState(device.status !== "暂停接活")
  const [limit, setLimit] = useState(String(device.max_sessions)), [busy, setBusy] = useState(false), [review, setReview] = useState(false)
  const [error, setError] = useState(""), [receipt, setReceipt] = useState<number | null>(null), [latest, setLatest] = useState<RDevice | null>(null)
  const [online, setOnline] = useState(navigator.onLine)
  useEffect(() => {
    const changed = () => setOnline(navigator.onLine)
    window.addEventListener("online", changed); window.addEventListener("offline", changed)
    return () => { window.removeEventListener("online", changed); window.removeEventListener("offline", changed) }
  }, [])
  const dirty = receive !== (base.status !== "暂停接活") || limit !== String(base.max_sessions)
  const enabled = available && device.connected === true && base.connected === true && device.can_configure === true && base.can_configure === true && !st.down && online && base.revision != null
  const cancel = () => { setReceive(base.status !== "暂停接活"); setLimit(String(base.max_sessions)); setError(""); setReceipt(null) }
  const readBack = async () => {
    setBusy(true)
    try {
      const result = await api.devices(), d = result.devices.find(d => d.id === device.id)
      if (!d) throw new Error(t("device:unknown"))
      setLatest(d); setBase(d); setReview(false); setError(t("device:checked"))
    } catch (e) { setError(String(e)) } finally { setBusy(false) }
  }
  const save = async () => {
    if (!enabled || base.revision == null) return
    setBusy(true); setError(""); setReceipt(null)
    try {
      const r = await api.patchDevice(device.id, { revision: base.revision, status: receive ? "正常" : "暂停接活", max_sessions: Number(limit) })
      if (r.device.id !== device.id || r.device.revision == null || r.device.revision <= base.revision) throw new Error("missing_receipt")
      setBase(r.device); setReceive(r.device.status !== "暂停接活"); setLimit(String(r.device.max_sessions)); setReceipt(r.device.revision); setLatest(null)
      void refresh()
    } catch (e) { setReview(true); setError(t(String(e).includes("device_revision_conflict") ? "device:conflict" : "device:saveUnknown")) }
    finally { setBusy(false) }
  }
  return <section className="device-config" data-device-config>
    <p>{t("device:configHelp")}</p><p>{t("device:unknownCapacity")}</p>
    {!device.can_configure && <p>{t(device.config_reason === "owner_required" ? "device:ownerOnly" : "device:unsupported")}</p>}
    <Switch label={t("device:receive")} checked={receive} onCheckedChange={setReceive} disabled={!enabled || busy} />
    <label>{t("device:limit")}<Input type="number" min={1} max={16} value={limit} onChange={e => setLimit(e.target.value)} disabled={!enabled || busy} data-concurrency /></label>
    <p>{base.revision != null ? t("device:revision", { revision: base.revision }) : t("device:unknown")}</p>
    {(!enabled || st.down) && <p>{t("device:offlineConfig")}</p>}
    {dirty && <p>{t("device:draft")}</p>}
    {error && <p role="alert">{error}</p>}
    {latest && <p data-config-latest>{t("device:latest", { status: latest.status === "暂停接活" ? t("device:paused") : t("device:configured"), limit: latest.max_sessions, revision: latest.revision })}</p>}
    {receipt != null && <p role="status" data-config-receipt>{t("device:saved", { revision: receipt })}</p>}
    <div className="device-actions"><Button onClick={() => void save()} disabled={!enabled || !dirty || busy || review || !/^\d+$/.test(limit) || Number(limit) < 1 || Number(limit) > 16} data-config-save>{t("common:action.save")}</Button>
      <Button variant="outline" onClick={cancel} disabled={busy} data-config-cancel>{t("common:action.cancel")}</Button>
      <Button variant="ghost" onClick={() => void readBack()} disabled={busy || st.down || !online} data-config-read>{t("device:readLatest")}</Button>
    </div>
  </section>
}
