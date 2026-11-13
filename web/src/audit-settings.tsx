/* 设置 · 操作记录: what was changed in this workspace and by whom, newest first: the Hub's audit trail, for the
 * instance's owner. An entry reads as a sentence (what · on what · who · when); the detail its writer kept opens
 * under it. An action this page has no words for goes by its code. */
import { useEffect, useState } from "react"
import { HistoryIcon } from "lucide-react"
import { HubError, modReq } from "@/api"
import { Button } from "@/components/ui/button"
import { st } from "@/store"
import { cn } from "cn"
import { Row } from "@/ui"
import { Panel, Section, Blank, Notice, Pill, Loading, Reveal, Turn } from "@/settings-kit"
import { t as tr, useT } from "@/i18n"
import { date, hm, monthDayTime } from "@/i18n/format"
import { actionKeys, changed, detailLines, purposeOf, type AuditEntry, type AuditPage } from "@/audit"

export const AUDIT_CAT = Object.defineProperties(["audit", "", HistoryIcon, ""] as [string, string, any, string], {
  1: { get: () => tr("manage:audit.title"), enumerable: true },
  3: { get: () => tr("manage:audit.sub"), enumerable: true },
})

const PAGE = 50
type Words = { names: Record<string, string>; purposes: Record<string, string> }

/** "新建 Bot「代码评审」": the action in words, with what it was done to when that has a name. An action on
 *  something of a `type` (a party that is a contact or an org) is worded for that type where there are words for it */
function title(e: AuditEntry, w: Words): string {
  const action = tr(actionKeys(e).map((k) => `manage:audit.action.${k}`), { defaultValue: e.action })
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
  if (typeof d.person === "string") parts.push(tr("manage:audit.person", { name: w.names[d.person] ?? d.person }))
  if (e.action.startsWith("onboard.")) {
    parts.push(...list(d.purposes).map(purpose), ...list(d.created))
    if (typeof d.how === "string") parts.push(tr(`manage:audit.how.${d.how}`, { defaultValue: d.how }))
  }
  parts.push(...changed(e).map(purposeOf).filter(Boolean).map(purpose))
  return parts.join(sep)
}

/** the day an entry is filed under: 今天, 昨天, or its date (with the year when it is not this one) */
function dayOf(ts: number) {
  const d = new Date(ts), today = new Date(); today.setHours(0, 0, 0, 0)
  const diff = Math.round((today.getTime() - new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime()) / 86400000)
  if (diff <= 0) return tr("manage:audit.today")
  if (diff === 1) return tr("manage:audit.yesterday")
  return date(d, { ...(d.getFullYear() !== today.getFullYear() && { year: "numeric" }), month: "short", day: "numeric" })
}
/** the entries by day, and within a day the runs of one person doing what reads as the same thing folded */
function byDay(xs: AuditEntry[], w: Words): [string, AuditEntry[][]][] {
  const out: [string, AuditEntry[][]][] = []
  for (const e of xs) {
    const d = dayOf(e.ts), day = out[out.length - 1]
    if (!day || day[0] !== d) { out.push([d, [[e]]]); continue }
    const run = day[1][day[1].length - 1], a = run[0]
    if (a.actor === e.actor && a.action === e.action && title(a, w) === title(e, w)) run.push(e); else day[1].push([e])
  }
  return out
}

export function AuditSettingsPane() {
  const t = useT()
  const [page, setPage] = useState<AuditPage | null>(null)
  const [err, setErr] = useState<string | null>(null), [denied, setDenied] = useState(false), [busy, setBusy] = useState(false)
  const [open, setOpen] = useState<number | null>(null), [unfold, setUnfold] = useState<Set<number>>(() => new Set())
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
  if (denied) return <Blank data-audit-denied>{t("manage:audit.ownerOnly")}</Blank>
  if (!page) return err
    ? <Notice tone="destructive" action={<Button size="xs" variant="ghost" onClick={() => void load()}>{t("common:action.retry")}</Button>}>{t("manage:audit.failed", { error: err })}</Notice>
    : <Loading className="py-12" />
  const w = { names: page.names, purposes }, last = page.entries[page.entries.length - 1]
  const entry = (e: AuditEntry, run?: AuditEntry[]) => {
    const sum = summary(e, w), who = w.names[e.actor] ?? e.actor, folded = !!run && run.length > 1
    const shut = folded && !unfold.has(e.id)
    const toggle = () => {
      if (folded) { const n = new Set(unfold); if (n.has(e.id)) n.delete(e.id); else n.add(e.id); setUnfold(n) }
      else setOpen(open === e.id ? null : e.id)
    }
    return <div key={e.id} data-audit-entry={e.action}>
      <Row title={<span className="flex min-w-0 items-center gap-2"><span className="truncate" data-audit-title>{title(e, w)}</span>{folded && <Pill className="tabular-nums" data-audit-fold={run!.length}>×{run!.length}</Pill>}</span>}
        sub={<span data-audit-sub>{[who, sum].filter(Boolean).join(" · ")}</span>} onClick={toggle} aria-expanded={folded ? !shut : open === e.id}>
        <span className="shrink-0 type-footnote tabular-nums text-muted-foreground">{folded ? `${hm(run![run!.length - 1].ts)} – ${hm(e.ts)}` : hm(e.ts)}</span><Turn open={folded ? !shut : open === e.id} flip />
      </Row>
      {folded ? <Reveal open={!shut}><div className="mb-2 ml-3 border-l pl-1">{run!.map((x) => entry(x))}</div></Reveal>
        : <Reveal open={open === e.id}><dl className="mx-3 mb-3 grid grid-cols-[max-content_minmax(0,1fr)] gap-x-4 gap-y-1.5 rounded-lg bg-muted/60 px-3.5 py-3 type-footnote" data-audit-detail>
          <dt className="text-muted-foreground">{t("manage:audit.when")}</dt><dd className="tabular-nums">{monthDayTime(e.ts)}</dd>
          <dt className="text-muted-foreground">{t("manage:audit.who")}</dt><dd className="break-all">{who}{who !== e.actor && <span className="ml-2 font-mono text-muted-foreground">{e.actor}</span>}</dd>
          <dt className="text-muted-foreground">{t("manage:audit.what")}</dt><dd className="break-all font-mono">{e.action}{e.target && <span className="ml-2 text-muted-foreground">{e.target}</span>}</dd>
          {detailLines(e, page.names).map(([k, v]) => {
            const purpose = purposes[purposeOf(k)]
            return <div key={k} className="contents"><dt className={cn("text-muted-foreground", !purpose && "font-mono")}>{purpose ?? k}</dt><dd className="break-words whitespace-pre-wrap">{v}</dd></div>
          })}
        </dl></Reveal>}
    </div>
  }
  return <div className="flex flex-col gap-8" data-audit>
    {page.entries.length === 0 && <Blank data-audit-empty>{t("manage:audit.empty")}</Blank>}
    {byDay(page.entries, w).map(([day, runs], n) => (
      <Section key={day} label={day} data-audit-day={day} note={n === 0 ? t("manage:audit.hint") : undefined}>
        <Panel>{runs.map((run) => entry(run[0], run))}</Panel>
      </Section>
    ))}
    {err && <Notice tone="destructive">{t("manage:audit.failed", { error: err })}</Notice>}
    {page.more && last && <Button size="sm" variant="outline" className="-mt-4 self-start" disabled={busy} onClick={() => void load(last.id)} data-audit-more>{busy ? t("manage:audit.loading") : t("manage:audit.more")}</Button>}
  </div>
}
