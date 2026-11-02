/* 邮箱设置 in the accounts page's AccountDetail (email.md §1, §9): signature, duty, send delay, archive when done, how far
 * back to sync. `GET /mail/accounts` → this connection's settings; each change is one `PATCH /mail/accounts/:id`.
 * On a Hub without the mail module the section isn't shown. */
import { useEffect, useState } from "react"
import { RefreshCwIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/ui/switch"
import { Textarea } from "@/components/ui/textarea"
import { Group, Row, SectionLabel, go } from "@/ui"
import { Select } from "@/views"
import { tm } from "@/store"
import { useLoad, useBusy } from "@/accounts/load"
import { mailApi, type MailAccount, type MailSettings } from "@/api/mail"

const DELAYS = [0, 10, 30, 60].map((s) => ({ v: String(s), l: s ? `${s} 秒` : "立即发出" }))
const DAYS = [7, 30, 90].map((d) => ({ v: String(d), l: `${d} 天` }))

function TextSetting({ id, label, sub, value, max, rows, onSave, busy }: { id: string; label: string; sub: string; value: string; max: number; rows: number; onSave: (v: string) => Promise<unknown>; busy: boolean }) {
  const [v, setV] = useState(value)
  useEffect(() => setV(value), [value])
  const dirty = v !== value
  return (
    <div className="flex flex-col gap-2 px-2 py-2.5">
      <label htmlFor={id} className="type-body">{label}<span className="mt-0.5 block type-footnote text-muted-foreground">{sub}</span></label>
      <Textarea id={id} className="rounded-lg" style={{ minHeight: rows * 22 + 16 }} value={v} maxLength={max} onChange={(e) => setV(e.target.value)} data-mail-setting={id} />
      {dirty && <div className="flex justify-end gap-2"><Button size="sm" variant="ghost" onClick={() => setV(value)}>还原</Button><Button size="sm" disabled={busy} onClick={() => void onSave(v)} data-mail-setting-save={id}>保存</Button></div>}
    </div>
  )
}

export function MailSettingsSection({ connection }: { connection: string }) {
  const L = useLoad(() => mailApi.accounts().then((xs) => xs.find((x) => x.connection_id === connection) ?? null), [connection], ["mail", "connection"])
  const [busy, act] = useBusy()
  const [a, setA] = useState<MailAccount | null>(null)
  useEffect(() => { if (L.s === "ok") setA(L.data) }, [L])
  if (L.s !== "ok" || !a) return null // no mail module on this Hub, or not a mailbox
  const s = a.settings
  const save = async (p: Partial<MailSettings>) => { const r = await act(() => mailApi.patchAccount(connection, p), "已保存"); if (r) setA(r) }
  return (
    <section data-mail-settings={connection}>
      <SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => go("#/mail?account=" + encodeURIComponent(connection))}>打开邮件</Button>}>邮箱设置</SectionLabel>
      <Group>
        <Row title={a.address || "邮箱"} sub={`${a.last_sync ? "上次同步 " + tm(a.last_sync) : "还没有同步"}${a.unread ? ` · ${a.unread} 封未读` : ""}`}>
          <Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void act(() => mailApi.sync(connection), "开始同步")} data-mail-sync><RefreshCwIcon />现在同步</Button></Row>
        <TextSetting id="signature" label="签名" sub="起草时附在正文末尾" value={s.signature} max={1000} rows={3} busy={busy} onSave={(v) => save({ signature: v })} />
        <TextSetting id="duty" label="职责与写信习惯" sub="这个邮箱用来做什么、回信时注意什么。起草时交给邮件 Bot" value={s.duty} max={4000} rows={3} busy={busy} onSave={(v) => save({ duty: v })} />
        <Row title="发送延迟" sub="批准后等这么久再发出，期间可以撤销"><Select align="end" testId="send-delay" value={String(s.send_delay_s)} onChange={(v) => void save({ send_delay_s: +v })} options={DELAYS.some((d) => d.v === String(s.send_delay_s)) ? DELAYS : [...DELAYS, { v: String(s.send_delay_s), l: `${s.send_delay_s} 秒` }]} /></Row>
        <Row title="事项完成后归档这串往来"><Switch aria-label="事项完成后归档这串往来" checked={s.archive_on_done} disabled={busy} onCheckedChange={(v) => void save({ archive_on_done: v })} data-archive-on-done /></Row>
        <Row title="回溯天数" sub="接入时读取多久以前的邮件；回溯的旧邮件只记录，不触发流程"><Select align="end" testId="sync-days" value={String(s.sync_days)} onChange={(v) => void save({ sync_days: +v })} options={DAYS} /></Row>
      </Group>
    </section>
  )
}
