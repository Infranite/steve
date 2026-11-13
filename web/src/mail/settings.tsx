/* 邮箱设置 inside a mailbox connection's settings card (email.md §1, §9): signature, duty, send delay, archive when done,
 * how far back to sync. `GET /mail/accounts` → this connection's settings. The switch and the two selects save at once,
 * as every other switch in the card; the two texts are typed, so their edits wait in a draft for the save bar right
 * under them and go out as one `PATCH /mail/accounts/:id` with only the fields changed since the edit began. On a Hub
 * without the mail module nothing is shown. */
import { useEffect, useState } from "react"
import { RefreshCwIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/ui/switch"
import { IntegrationRow, IntegrationSaveBar, IntegrationSection, IntegrationTextarea } from "@/components/affect/integration-settings-clean"
import { go } from "@/ui"
import { Select } from "@/view-shared"
import { tm } from "@/store"
import { useLoad, useBusy } from "@/accounts/load"
import { mailApi, type MailAccount, type MailSettings } from "@/api/mail"
import { useT } from "@/i18n"

type T = ReturnType<typeof useT>
const delays = (t: T) => [0, 10, 30, 60].map((s) => ({ v: String(s), l: s ? t("common:dur.seconds", { count: s }) : t("mail:settings.sendNow") }))
const days = (t: T) => [7, 30, 90].map((d) => ({ v: String(d), l: t("common:dur.days", { count: d }) }))
type Texts = Pick<MailSettings, "signature" | "duty">
const TEXTS = ["signature", "duty"] as const
const texts = (s: MailSettings): Texts => ({ signature: s.signature, duty: s.duty })

export type MailSettingsState = ReturnType<typeof useMailSettings>

/** the mailbox behind `connection` (null: not a mailbox, or no mail module), and the draft of its two texts */
export function useMailSettings(connection: string | null) {
  const t = useT()
  const L = useLoad(() => (connection ? mailApi.accounts().then((xs) => xs.find((x) => x.connection_id === connection) ?? null) : Promise.resolve(null)), [connection], ["mail", "connection"])
  const [busy, act] = useBusy()
  const [a, setA] = useState<MailAccount | null>(null)
  // `base`: the texts when the edit began; a reload from the stream refreshes `a` but never the edit in progress
  const [edit, setEdit] = useState<{ base: Texts; draft: Texts } | null>(null)
  useEffect(() => { if (L.s === "ok") setA(L.data) }, [L])
  const changed = edit ? TEXTS.filter((k) => edit.draft[k] !== edit.base[k]) : []
  const dirty = changed.length > 0
  // leaving the page (reload, close) with typed text unsaved asks first
  useEffect(() => {
    if (!dirty) return
    const warn = (e: BeforeUnloadEvent) => { e.preventDefault(); e.returnValue = "" }
    window.addEventListener("beforeunload", warn)
    return () => window.removeEventListener("beforeunload", warn)
  }, [dirty])
  const values: Texts | null = edit?.draft ?? (a ? texts(a.settings) : null)
  const type = (p: Partial<Texts>) => setEdit((e) => (e ? { ...e, draft: { ...e.draft, ...p } } : a ? { base: texts(a.settings), draft: { ...texts(a.settings), ...p } } : e))
  const save = async () => {
    if (!edit || !connection || !dirty) return
    const r = await act(() => mailApi.patchAccount(connection, Object.fromEntries(changed.map((k) => [k, edit.draft[k]]))), t("common:state.saved"))
    if (r) { setA(r); setEdit(null) }
  }
  /** a switch or a select: saved at once */
  const put = async (p: Partial<MailSettings>) => {
    if (!connection) return
    const r = await act(() => mailApi.patchAccount(connection, p), t("common:state.saved"))
    if (r) setA(r)
  }
  const discard = () => setEdit(null)
  const sync = () => { if (connection) void act(() => mailApi.sync(connection), t("mail:settings.syncStarted")) }
  return { a, values, dirty, busy, type, put, save, discard, sync }
}

export function MailSettingsSections({ m, connection }: { m: MailSettingsState; connection: string }) {
  const t = useT()
  const { a, values: v } = m
  if (!a || !v) return null
  const s = a.settings
  const DELAYS = delays(t)
  const sync = a.last_sync ? t("mail:settings.lastSync", { time: tm(a.last_sync) }) : t("mail:settings.neverSynced")
  return (
    <div data-mail-settings={connection}>
      <IntegrationSection label={t("mail:settings.title")}>
        <IntegrationRow title={a.address || t("mail:settings.mailbox")} hint={a.unread ? t("mail:settings.syncUnread", { sync, count: a.unread }) : sync}>
          <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={m.busy} onClick={m.sync} data-mail-sync><RefreshCwIcon />{t("mail:settings.syncNow")}</Button>
          <Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => go("#/mail?account=" + encodeURIComponent(connection))}>{t("mail:settings.openMail")}</Button>
        </IntegrationRow>
        <IntegrationRow title={t("mail:settings.sendDelay")} hint={t("mail:settings.sendDelaySub")}>
          <Select align="end" testId="send-delay" value={String(s.send_delay_s)} onChange={(x) => void m.put({ send_delay_s: +x })} options={DELAYS.some((d) => d.v === String(s.send_delay_s)) ? DELAYS : [...DELAYS, { v: String(s.send_delay_s), l: t("common:dur.seconds", { count: s.send_delay_s }) }]} />
        </IntegrationRow>
        <IntegrationRow title={t("mail:settings.archiveOnDone")}>
          <Switch aria-label={t("mail:settings.archiveOnDone")} checked={s.archive_on_done} disabled={m.busy} onCheckedChange={(x) => void m.put({ archive_on_done: x })} data-archive-on-done />
        </IntegrationRow>
        <IntegrationRow title={t("mail:settings.syncDays")} hint={t("mail:settings.syncDaysSub")}>
          <Select align="end" testId="sync-days" value={String(s.sync_days)} onChange={(x) => void m.put({ sync_days: +x })} options={days(t)} />
        </IntegrationRow>
      </IntegrationSection>
      <IntegrationTextarea label={t("mail:settings.signature")} hint={t("mail:settings.signatureSub")} value={v.signature} maxLength={1000} rows={3} onChange={(x) => m.type({ signature: x })} data-mail-setting="signature" />
      <IntegrationTextarea label={t("mail:settings.duty")} hint={t("mail:settings.dutySub")} value={v.duty} maxLength={4000} rows={3} onChange={(x) => m.type({ duty: x })} data-mail-setting="duty" />
      <IntegrationSaveBar dirty={m.dirty} saving={m.busy} onSave={() => void m.save()} onDiscard={m.discard}
        labels={{ unsaved: t("mail:settings.unsaved"), discard: t("mail:settings.discard"), save: t("common:action.save"), saving: t("mail:settings.saving") }} />
    </div>
  )
}
