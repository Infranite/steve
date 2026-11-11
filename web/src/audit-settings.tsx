/* 设置 · 操作记录: what was changed in this workspace and by whom, newest first: the Hub's audit trail, for the
 * instance's owner. An entry reads as a sentence (what · on what · who · when); the detail its writer kept opens
 * under it. An action this page has no words for goes by its code. */
import { useEffect, useState } from "react"
import { HistoryIcon } from "lucide-react"
import { HubError, modReq } from "@/api"
import { Button } from "@/components/ui/button"
import { Spinner } from "@/components/ui/spinner"
import { st, tm } from "@/store"
import { Group, Row } from "@/ui"
import { t as tr, useT } from "@/i18n"
import { monthDayTime } from "@/i18n/format"
import { actionKey, changed, detailLines, purposeOf, type AuditEntry, type AuditPage } from "@/audit"

export const AUDIT_CAT = Object.defineProperties(["audit", "", HistoryIcon, ""] as [string, string, any, string], {
  1: { get: () => tr("manage:audit.title"), enumerable: true },
  3: { get: () => tr("manage:audit.sub"), enumerable: true },
})

const PAGE = 50
type Words = { names: Record<string, string>; purposes: Record<string, string> }

/** "新建 Bot「代码评审」": the action in words, with what it was done to when that has a name */
function title(e: AuditEntry, w: Words): string {
  const action = tr(`manage:audit.action.${actionKey(e.action)}`, { defaultValue: e.action })
  const target = e.target === st.workspace?.id ? "" : w.names[e.target] ?? ""
  return target ? tr("manage:audit.withTarget", { action, target }) : action
}
/** what an entry changed, in a few words: the profile's parts, the purposes, the Bots; "" when the title says it all */
function summary(e: AuditEntry, w: Words): string {
  const d = e.detail ?? {}, sep = tr("shell:list.sep")
  const list = (v: unknown) => (Array.isArray(v) ? v.map(String) : [])
  const purpose = (k: string) => w.purposes[k] ?? k
  const fields = list(d.fields).map((f) => tr(`manage:audit.field.${f}`, { defaultValue: f }))
  const parts: string[] = []
  if (fields.length) parts.push(fields.join(sep))
  if (typeof d.memories === "number" && d.memories > 0) parts.push(tr("manage:audit.memories", { count: d.memories }))
  if (e.action.startsWith("onboard.")) {
    parts.push(...list(d.purposes).map(purpose), ...list(d.created))
    if (typeof d.how === "string") parts.push(tr(`manage:audit.how.${d.how}`, { defaultValue: d.how }))
  }
  parts.push(...changed(e).map(purposeOf).filter(Boolean).map(purpose))
  return parts.join(sep)
}

export function AuditSettingsPane() {
  const t = useT()
  const [page, setPage] = useState<AuditPage | null>(null)
  const [err, setErr] = useState<string | null>(null), [denied, setDenied] = useState(false), [busy, setBusy] = useState(false)
  const [open, setOpen] = useState<number | null>(null)
  const [purposes, setPurposes] = useState<Record<string, string>>({})
  const load = (before?: number) => {
    setBusy(true); setErr(null)
    return modReq<AuditPage>("GET", `/audit?limit=${PAGE}${before ? `&before=${before}` : ""}`)
      .then((p) => setPage((old) => (before && old ? { entries: [...old.entries, ...p.entries], names: { ...old.names, ...p.names }, more: p.more } : p)))
      .catch((e) => { if (e instanceof HubError && e.status === 403) setDenied(true); else setErr(e instanceof Error ? e.message : String(e)) })
      .finally(() => setBusy(false))
  }
  useEffect(() => {
    void load()
    /* the purposes by the names the models page gives them; without them an entry says the purpose's key */
    modReq<{ purposes: { key: string; label: string }[] }>("GET", "/settings/models").then((v) => setPurposes(Object.fromEntries(v.purposes.map((p) => [p.key, p.label])))).catch(() => {})
  }, [])
  if (denied) return <p className="type-callout text-muted-foreground" data-audit-denied>{t("manage:audit.ownerOnly")}</p>
  if (!page) return err
    ? <p className="type-callout text-muted-foreground">{t("manage:audit.failed", { error: err })}<Button size="xs" variant="ghost" className="ml-2" onClick={() => void load()}>{t("common:action.retry")}</Button></p>
    : <p className="flex items-center gap-2 type-callout text-muted-foreground" data-audit-loading><Spinner className="size-3.5" />{t("manage:audit.loading")}</p>
  const w = { names: page.names, purposes }, last = page.entries[page.entries.length - 1]
  return <section className="flex flex-col gap-3" data-audit>
    <p className="type-footnote text-muted-foreground">{t("manage:audit.hint")}</p>
    {page.entries.length === 0 && <p className="type-callout text-muted-foreground" data-audit-empty>{t("manage:audit.empty")}</p>}
    <Group>
      {page.entries.map((e) => {
        const sum = summary(e, w), who = w.names[e.actor] ?? e.actor
        return <div key={e.id} data-audit-entry={e.action}>
          <Row title={<span data-audit-title>{title(e, w)}</span>} sub={<span data-audit-sub>{[who, sum].filter(Boolean).join(" · ")}</span>} onClick={() => setOpen(open === e.id ? null : e.id)} aria-expanded={open === e.id}>
            <span className="shrink-0 type-footnote tabular-nums text-muted-foreground">{tm(e.ts)}</span>
          </Row>
          {open === e.id && <dl className="mb-2 ml-2 grid grid-cols-[max-content_minmax(0,1fr)] gap-x-4 gap-y-1 rounded-lg bg-muted/50 px-3 py-2.5 type-footnote" data-audit-detail>
            <dt className="text-muted-foreground">{t("manage:audit.when")}</dt><dd className="tabular-nums">{monthDayTime(e.ts)}</dd>
            <dt className="text-muted-foreground">{t("manage:audit.who")}</dt><dd className="break-all">{who}{who !== e.actor && <span className="ml-2 font-mono text-tertiary">{e.actor}</span>}</dd>
            <dt className="text-muted-foreground">{t("manage:audit.what")}</dt><dd className="break-all font-mono">{e.action}{e.target && <span className="ml-2 text-tertiary">{e.target}</span>}</dd>
            {detailLines(e, page.names).map(([k, v]) => <div key={k} className="contents"><dt className="font-mono text-muted-foreground">{k}</dt><dd className="break-words whitespace-pre-wrap">{v}</dd></div>)}
          </dl>}
        </div>
      })}
    </Group>
    {err && <p className="type-callout text-muted-foreground">{t("manage:audit.failed", { error: err })}</p>}
    {page.more && last && <Button size="sm" variant="outline" className="self-start" disabled={busy} onClick={() => void load(last.id)} data-audit-more>{busy ? t("manage:audit.loading") : t("manage:audit.more")}</Button>}
  </section>
}
