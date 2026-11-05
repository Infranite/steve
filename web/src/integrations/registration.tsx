import { useEffect, useRef, useState } from "react"
import { ExternalLinkIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { channelsApi, type FeishuRegistration } from "@/api/channels"
import { vaultApi } from "@/api/vault"
import { isNotWired } from "@/api"
import { go } from "@/ui"
import { Select } from "@/view-shared"
import { t as tr, useT } from "@/i18n"
import { officialConfirmationUrl, registrationLabel, registrationPending, remainingSeconds } from "./registration-state"

const errorText = (e: unknown) => e instanceof Error ? e.message : tr("integrations:reg.connectFailed")

/** A server-owned attempt survives navigation. Reopening this page never starts an application. */
export function FeishuRegistrationPanel() {
  const t = useT()
  const [brand, setBrand] = useState<"feishu" | "lark">("feishu")
  const [r, setRegistration] = useState<FeishuRegistration | null>(null)
  const [loading, setLoading] = useState(true), [busy, setBusy] = useState(false)
  const [vaultReady, setVaultReady] = useState<boolean | null>(null)
  const [error, setError] = useState(""), [unavailable, setUnavailable] = useState(false)
  const [now, setNow] = useState(Date.now()), [reload, setReload] = useState(0)
  const epoch = useRef(0), mounted = useRef(true)
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; epoch.current++ } }, [])
  useEffect(() => {
    let stop = false
    setLoading(true)
    channelsApi.registrationCurrent().then((next) => {
      if (!stop) { setRegistration(next); setError(""); setUnavailable(false) }
    }).catch((e) => {
      if (!stop) { setUnavailable(isNotWired(e)); setError(isNotWired(e) ? t("integrations:reg.unavailable") : errorText(e)) }
    }).finally(() => { if (!stop) setLoading(false) })
    return () => { stop = true }
  }, [reload])
  useEffect(() => {
    let stop = false
    vaultApi.status().then((s) => { if (!stop) setVaultReady(s.initialized) }, () => {})
    return () => { stop = true }
  }, [reload])
  const pending = r ? registrationPending(r) : false
  useEffect(() => {
    if (!r || !pending) return
    let stop = false, timer: ReturnType<typeof setTimeout>
    const id = r.id, interval = Math.max(2, Math.min(60, r.interval || 5)) * 1000
    const tick = async () => {
      const version = epoch.current
      try {
        const next = await channelsApi.registration(id)
        if (!stop && version === epoch.current) { setRegistration(next); setError("") }
      } catch (e) { if (!stop && version === epoch.current) setError(t("integrations:reg.interrupted", { error: errorText(e) })) }
      if (!stop) timer = setTimeout(tick, interval)
    }
    timer = setTimeout(tick, interval)
    const clock = setInterval(() => setNow(Date.now()), 1000)
    return () => { stop = true; clearTimeout(timer); clearInterval(clock) }
  }, [r?.id, r?.state, r?.error, r?.interval, pending])

  const act = async (fn: () => Promise<FeishuRegistration>) => {
    if (busy) return
    ++epoch.current // a poll started before this mutation cannot overwrite its result
    setBusy(true); setError("")
    try {
      const next = await fn()
      if (mounted.current) { setRegistration(next); setNow(Date.now()) }
    } catch (e) { if (mounted.current) setError(errorText(e)) }
    finally { ++epoch.current; if (mounted.current) setBusy(false) }
  }
  const url = officialConfirmationUrl(r?.verification_uri_complete)
  const left = remainingSeconds(r?.expires_at ?? 0, now)
  const startAllowed = vaultReady !== false && !loading && !busy && !unavailable && !error && !pending && r?.state !== "credentials_saved"
  const terminal = r && !pending && r.state !== "credentials_saved" && r.state !== "succeeded"
  return <div className="flex flex-col gap-3 rounded-xl border p-4" data-app-registration>
    <div className="type-body font-medium">{t("integrations:reg.title")}</div>
    <p className="type-callout text-muted-foreground">{t("integrations:reg.intro")}</p>
    <p className="type-footnote text-muted-foreground">{t("integrations:reg.fit")}</p>
    {vaultReady === false && <div role="status" className="flex flex-wrap items-center gap-2 type-callout">{t("integrations:reg.vaultFirst")}<Button variant="outline" onClick={() => go("#/vault")}>{t("integrations:feishu.vaultGo")}</Button></div>}
    {loading ? <div role="status" className="flex items-center gap-2 type-callout"><Spinner />{t("integrations:reg.restoring")}</div> : r && <div className="flex flex-col gap-2 rounded-lg bg-muted/50 p-3" data-registration-state={r.state}>
      <div role="status" className="flex items-center gap-2 type-callout font-medium">{pending && <Spinner />}{registrationLabel(r)}</div>
      {r.state === "pending" && <>
        {url && <Button nativeButton={false} render={<a href={url} target="_blank" rel="noreferrer noopener" />} data-registration-link><ExternalLinkIcon />{t("integrations:reg.openConfirm")}</Button>}
        {!url && <p role="alert" className="type-callout text-destructive">{t("integrations:reg.noUrl")}</p>}
        {r.user_code && <div className="type-callout">{t("integrations:reg.userCode")}<code className="select-all font-mono">{r.user_code}</code></div>}
        <p className="type-footnote text-muted-foreground">{left ? t("integrations:reg.linkLeft", { left: t("common:dur.minSec", { m: Math.floor(left / 60), s: left % 60 }) }) : t("integrations:reg.linkChecking")}</p>
      </>}
      {r.app_may_exist && r.state !== "cancelled" && <p className="type-footnote text-muted-foreground">{t("integrations:reg.mayExist")}</p>}
      {r.error && <p role="alert" className="break-words type-callout text-destructive">{r.error}</p>}
      {r.state === "credentials_saved" && r.error && <Button variant="outline" disabled={busy} onClick={() => void act(() => channelsApi.registrationRetry(r.id))}>{t("integrations:reg.retryCheck")}</Button>}
      {r.state === "succeeded" && <>
        <p className="type-callout text-muted-foreground">{t("integrations:reg.done")}</p>
        {r.connection_id && <Button onClick={() => go("#/channels/acc/" + r.connection_id)}>{t("integrations:reg.viewConnection")}</Button>}
      </>}
      {(pending || r.state === "credentials_saved") && <>
        <p className="type-footnote text-muted-foreground">{t("integrations:reg.leaveNote")}</p>
        <Button variant="outline" disabled={busy} onClick={() => void act(() => channelsApi.registrationCancel(r.id))}>{t("integrations:reg.cancel")}</Button>
      </>}
      {r.state === "cancelled" && <p className="type-footnote text-muted-foreground">{t("integrations:reg.cancelledNote")}</p>}
    </div>}
    {error && <div role="alert" className="flex flex-col items-start gap-2 type-callout text-destructive">{error}
      {<Button size="sm" variant="outline" disabled={busy || loading} onClick={() => { ++epoch.current; setReload((x) => x + 1) }}>{t("integrations:reg.reload")}</Button>}
    </div>}
    {!pending && r?.state !== "credentials_saved" && <div className="flex flex-wrap items-center gap-2">
      <Select testId="registration-brand" value={brand} onChange={(v) => setBrand(v as "feishu" | "lark")} options={[{ v: "feishu", l: t("integrations:feishu.brand") }, { v: "lark", l: "Lark" }]} />
      <Button disabled={!startAllowed} onClick={() => void act(() => channelsApi.registrationStart(brand))} data-registration-start>{busy && <Spinner />}{terminal ? t("integrations:reg.restart") : r?.state === "succeeded" ? t("integrations:reg.another") : t("integrations:reg.start")}</Button>
    </div>}
    {terminal && <p className="type-footnote text-muted-foreground">{t("integrations:reg.restartNote")}</p>}
  </div>
}
