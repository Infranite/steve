/* 邮箱设置 in the accounts page's AccountDetail (email.md §1, §9): signature, duty, send delay, archive when done, how far
 * back to sync. `GET /mail/accounts` → this connection's settings; each change is one `PATCH /mail/accounts/:id`.
 * On a Hub without the mail module the section isn't shown. */
import { useEffect, useState } from "react"
import { RefreshCwIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/ui/switch"
import { Textarea } from "@/components/ui/textarea"
import { Group, Row, SectionLabel, go } from "@/ui"
import { Select } from "@/view-shared"
import { tm } from "@/store"
import { useLoad, useBusy } from "@/accounts/load"
import { mailApi, type MailAccount, type MailSettings } from "@/api/mail"
import { useT } from "@/i18n"

type T = ReturnType<typeof useT>
const delays = (t: T) => [0, 10, 30, 60].map((s) => ({ v: String(s), l: s ? t("common:dur.seconds", { count: s }) : t("mail:settings.sendNow") }))
const days = (t: T) => [7, 30, 90].map((d) => ({ v: String(d), l: t("common:dur.days", { count: d }) }))

function TextSetting({ id, label, sub, value, max, rows, onSave, busy }: { id: string; label: string; sub: string; value: string; max: number; rows: number; onSave: (v: string) => Promise<unknown>; busy: boolean }) {
  const t = useT()
  const [v, setV] = useState(value)
  useEffect(() => setV(value), [value])
  const dirty = v !== value
  return (
    <div className="flex flex-col gap-2 px-2 py-2.5">
      <label htmlFor={id} className="type-body">{label}<span className="mt-0.5 block type-footnote text-muted-foreground">{sub}</span></label>
      <Textarea id={id} className="rounded-lg" style={{ minHeight: rows * 22 + 16 }} value={v} maxLength={max} onChange={(e) => setV(e.target.value)} data-mail-setting={id} />
      {dirty && <div className="flex justify-end gap-2"><Button size="sm" variant="ghost" onClick={() => setV(value)}>{t("mail:settings.revert")}</Button><Button size="sm" disabled={busy} onClick={() => void onSave(v)} data-mail-setting-save={id}>{t("common:action.save")}</Button></div>}
    </div>
  )
}

export function MailSettingsSection({ connection }: { connection: string }) {
  const t = useT()
  const L = useLoad(() => mailApi.accounts().then((xs) => xs.find((x) => x.connection_id === connection) ?? null), [connection], ["mail", "connection"])
  const [busy, act] = useBusy()
  const [a, setA] = useState<MailAccount | null>(null)
  useEffect(() => { if (L.s === "ok") setA(L.data) }, [L])
  if (L.s !== "ok" || !a) return null // no mail module on this Hub, or not a mailbox
  const s = a.settings
  const save = async (p: Partial<MailSettings>) => { const r = await act(() => mailApi.patchAccount(connection, p), t("common:state.saved")); if (r) setA(r) }
  const DELAYS = delays(t)
  const sync = a.last_sync ? t("mail:settings.lastSync", { time: tm(a.last_sync) }) : t("mail:settings.neverSynced")
  return (
    <section data-mail-settings={connection}>
      <SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => go("#/mail?account=" + encodeURIComponent(connection))}>{t("mail:settings.openMail")}</Button>}>{t("mail:settings.title")}</SectionLabel>
      <Group>
        <Row title={a.address || t("mail:settings.mailbox")} sub={a.unread ? t("mail:settings.syncUnread", { sync, count: a.unread }) : sync}>
          <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void act(() => mailApi.sync(connection), t("mail:settings.syncStarted"))} data-mail-sync><RefreshCwIcon />{t("mail:settings.syncNow")}</Button></Row>
        <TextSetting id="signature" label={t("mail:settings.signature")} sub={t("mail:settings.signatureSub")} value={s.signature} max={1000} rows={3} busy={busy} onSave={(v) => save({ signature: v })} />
        <TextSetting id="duty" label={t("mail:settings.duty")} sub={t("mail:settings.dutySub")} value={s.duty} max={4000} rows={3} busy={busy} onSave={(v) => save({ duty: v })} />
        <Row title={t("mail:settings.sendDelay")} sub={t("mail:settings.sendDelaySub")}><Select align="end" testId="send-delay" value={String(s.send_delay_s)} onChange={(v) => void save({ send_delay_s: +v })} options={DELAYS.some((d) => d.v === String(s.send_delay_s)) ? DELAYS : [...DELAYS, { v: String(s.send_delay_s), l: t("common:dur.seconds", { count: s.send_delay_s }) }]} /></Row>
        <Row title={t("mail:settings.archiveOnDone")}><Switch aria-label={t("mail:settings.archiveOnDone")} checked={s.archive_on_done} disabled={busy} onCheckedChange={(v) => void save({ archive_on_done: v })} data-archive-on-done /></Row>
        <Row title={t("mail:settings.syncDays")} sub={t("mail:settings.syncDaysSub")}><Select align="end" testId="sync-days" value={String(s.sync_days)} onChange={(v) => void save({ sync_days: +v })} options={days(t)} /></Row>
      </Group>
    </section>
  )
}
