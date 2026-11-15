import { useEffect, useRef, useState } from "react"
import { CopyIcon, LinkIcon, RefreshCwIcon, Trash2Icon } from "lucide-react"
import { api } from "@/api"
import { currentHub } from "@/hub"
import { Card, CardContent, CardHeader } from "@/components/affect/card-clean"
import { Select } from "@/components/affect/select-clean"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { date } from "@/i18n/format"
import { useT } from "@/i18n"
import { loopbackUrl, webUrl, type DeliveryShare } from "@/delivery"

export function DeliveryShares({ id }: { id: string }) {
  const t = useT(), [shares, setShares] = useState<DeliveryShare[] | null>(null)
  const [error, setError] = useState(""), [busy, setBusy] = useState(false), [duration, setDuration] = useState("86400000")
  const [copied, setCopied] = useState(""), [now, setNow] = useState(Date.now), generation = useRef(0)
  const [loading, setLoading] = useState(false)
  const load = async () => {
    const g = ++generation.current
    setError(""); setLoading(true)
    try { const rows = await api.deliveryShares(id); if (!Array.isArray(rows)) throw new Error(t("pages:delivery.shareLoadError")); if (g === generation.current) setShares(rows) }
    catch (e) { if (g === generation.current) setError((e as Error).message || t("pages:delivery.shareLoadError")) }
    finally { if (g === generation.current) setLoading(false) }
  }
  useEffect(() => {
    setShares(null); void load()
    const tick = window.setInterval(() => setNow(Date.now()), 1000)
    return () => { generation.current++; clearInterval(tick) }
  }, [id]) // id is the ownership boundary; late results from another delivery must not populate this panel
  const create = async () => {
    if (busy || loading) return
    setBusy(true); setError("")
    const g = generation.current
    try {
      const s = await api.createDeliveryShare(id, Date.now() + Number(duration))
      if (g === generation.current) { setShares((old) => [s, ...(old ?? []).filter((x) => x.token !== s.token)]); setNow(Date.now()) }
    } catch (e) { if (g === generation.current) setError((e as Error).message) }
    finally { if (g === generation.current) setBusy(false) }
  }
  const revoke = async (token: string) => {
    if (busy || loading) return
    setBusy(true); setError("")
    const g = generation.current
    try {
      await api.revokeDeliveryShare(id, token)
      if (g === generation.current) setShares((old) => old?.map((s) => s.token === token ? { ...s, revoked_at: Date.now() } : s) ?? [])
    } catch (e) { if (g === generation.current) setError((e as Error).message) }
    finally { if (g === generation.current) setBusy(false) }
  }
  const copy = async (url: string, token: string) => {
    try { await navigator.clipboard.writeText(url); setCopied(token) }
    catch { setError(t("pages:delivery.copyFailed")) }
  }
  return <Card as="section" data-delivery-shares>
    <CardHeader title={t("pages:delivery.sharing")} titleAs="h2" description={t("pages:delivery.shareHint")} icon={<LinkIcon />} />
    <CardContent className="flex flex-col gap-3">
      <div className="flex flex-wrap items-end gap-2">
        <Select data-share-duration className="min-w-0 max-w-full" label={t("pages:delivery.validFor")} value={duration} onChange={setDuration} disabled={busy} options={[
          { value: "3600000", label: t("pages:delivery.oneHour") },
          { value: "86400000", label: t("pages:delivery.oneDay") },
          { value: "604800000", label: t("pages:delivery.oneWeek") },
        ]} />
        <Button onClick={() => void create()} disabled={busy || loading || shares === null} data-share-create>{busy ? <Spinner /> : <LinkIcon />}{t("pages:delivery.createShare")}</Button>
        <Button size="sm" variant="ghost" disabled={busy || loading} onClick={() => void load()} data-shares-refresh><RefreshCwIcon />{t("common:action.refresh")}</Button>
      </div>
      {error && <p className="wrap-anywhere type-callout text-destructive" role="alert" data-share-error>{error}</p>}
      {!shares && !error && <Spinner className="size-4" />}
      {shares?.length === 0 && <p className="type-callout text-muted-foreground">{t("pages:delivery.noShares")}</p>}
      <ul className="flex min-w-0 flex-col gap-3">
        {shares?.map((s) => {
          const url = webUrl(s.url, currentHub()), expired = s.expires_at <= now, active = !s.revoked_at && !expired
          return <li key={s.token} className="flex min-w-0 flex-col gap-1.5 rounded-lg bg-muted/50 p-3" data-share-state={s.revoked_at ? "revoked" : expired ? "expired" : "active"}>
            <p className="type-footnote text-muted-foreground">{s.revoked_at ? t("pages:delivery.revoked") : t(expired ? "pages:delivery.expiredAt" : "pages:delivery.expiresAt", { time: date(s.expires_at, { dateStyle: "medium", timeStyle: "short" }) })}</p>
            {active && <>
              {url ? <a href={url} target="_blank" rel="noreferrer" className="wrap-anywhere type-callout underline underline-offset-2" data-share-url>{url}</a> : <p role="alert">{t("pages:delivery.invalidUrl")}</p>}
              {url && loopbackUrl(url) && <p className="type-footnote text-attention" data-loopback-warning>{t("pages:delivery.loopback")}</p>}
              <div className="flex flex-wrap gap-2">
                <Button size="sm" variant="outline" disabled={!url} onClick={() => url && void copy(url, s.token)} data-share-copy><CopyIcon />{copied === s.token ? t("pages:delivery.copied") : t("pages:delivery.copyLink")}</Button>
                <Button size="sm" variant="ghost" disabled={busy || loading} onClick={() => void revoke(s.token)} data-share-revoke><Trash2Icon />{t("pages:delivery.revoke")}</Button>
              </div>
            </>}
          </li>
        })}
      </ul>
    </CardContent>
  </Card>
}
