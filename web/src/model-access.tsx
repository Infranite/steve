/* Model access for service sessions (#222): which provider endpoint, and which key from the Hub's shared vault, the
 * sessions of a workspace's service principal (schedules, flows) use per harness. They never use anyone's personal
 * account, so without it they cannot reach a model. Settings · Models edits it (instance owner only); the schedules
 * page shows which principals and Bots cannot run right now and where that is fixed. */
import { useEffect, useState } from "react"
import { TriangleAlertIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { modReq, isNotWired } from "@/api"
import { st, toast } from "@/store"
import { runtimeSetupHrefFor } from "@/hub-management-model"
import { Section, Panel, Row, Notice, Pill } from "@/ui"
import { SetSelect } from "@/settings-controls"
import { HarnessMark } from "@/components/harness-mark"
import { useT } from "@/i18n"

export type Problem = "model_access_missing" | "credential_missing" | "credential_not_shared" | "harness_not_ready"
export type Fix = "settings.model_access" | "devices" | "vault"
export type Blocked = { workspace: string; workspace_name: string; principal: string; bot: string; bot_name: string; harness: string; code: Problem; reason: string; fix: Fix }
export type AccessHarness = { id: string; name: string; base_url: string | null; item: string | null; field: string | null; updated: number | null; required: boolean; problem: Problem | null }
export type AccessItem = { id: string; title: string; scope: "shared" | "system"; fields: string[] }
export type ModelAccess = { harnesses: AccessHarness[]; items: AccessItem[]; blocked: Blocked[]; principal_state: boolean; can_edit: boolean }

export const modelAccessApi = {
  get: () => modReq<ModelAccess>("GET", "/model-access"),
  put: (harness: string, b: { base_url: string; item: string; field: string }) => modReq<ModelAccess>("PUT", `/model-access/${encodeURIComponent(harness)}`, b),
  clear: (harness: string) => modReq<ModelAccess>("DELETE", `/model-access/${encodeURIComponent(harness)}`),
}

/** where each kind of trouble is fixed */
/** "devices" means a harness is not ready: that is set up on the Hub's own Node page, not the devices overview */
export const fixHref = (f: Fix) => (f === "devices" ? runtimeSetupHrefFor(st.devices) : f === "vault" ? "#/vault" : "#/settings/models")

/** the setting, loaded once per mount; `null` while loading, `undefined` where the Hub does not serve it */
export function useModelAccess(): [ModelAccess | null | undefined, (v: ModelAccess) => void] {
  const [v, setV] = useState<ModelAccess | null | undefined>(null)
  useEffect(() => {
    let live = true
    modelAccessApi.get().then((x) => { if (live) setV(x) }, (e) => { if (live) setV(isNotWired(e) ? undefined : null) })
    return () => { live = false }
  }, [])
  return [v, setV]
}

/** the principals and Bots that cannot run now, each with its reason and a link to where it is fixed */
export function BlockedList({ rows, compact }: { rows: Blocked[]; compact?: boolean }) {
  const t = useT()
  if (!rows.length) return null
  return (
    <Notice tone="attention" role="status" data-model-access-blocked={rows.length}>
      <div className="flex flex-col gap-1.5">
        <span className="font-medium">{t("pages:modelAccess.blockedTitle", { count: rows.length })}</span>
        <ul className="flex flex-col gap-1">{rows.slice(0, compact ? 5 : rows.length).map((r) => (
          <li key={r.principal + r.bot} className="type-footnote" data-blocked-bot={r.bot} data-blocked-principal={r.principal}>
            <span className="font-medium">{t("pages:modelAccess.blockedWho", { bot: r.bot_name, workspace: r.workspace_name })}</span>
            <span className="text-muted-foreground"> · {r.reason} </span>
            <a href={fixHref(r.fix)} className="whitespace-nowrap text-blue hover:underline" data-blocked-fix={r.fix}>{t(`pages:modelAccess.fix.${r.fix === "settings.model_access" ? "settings" : r.fix}`)}</a>
          </li>
        ))}</ul>
        {compact && rows.length > 5 && <a href="#/settings/models" className="type-footnote text-blue hover:underline">{t("pages:modelAccess.blockedMore", { count: rows.length - 5 })}</a>}
      </div>
    </Notice>
  )
}

/** on the schedules and devices pages: the list alone, nothing when everything can run */
export function ModelAccessBlocked({ compact = true }: { compact?: boolean }) {
  const [v] = useModelAccess()
  if (!v) return null
  return <BlockedList rows={v.blocked} compact={compact} />
}

/** Settings · Models: the provider of each harness for service sessions */
export function ModelAccessSection() {
  const t = useT()
  const [v, setV] = useModelAccess()
  const [open, setOpen] = useState<string | null>(null)
  if (v === undefined || v === null) return null
  return (
    <Section data-model-access label={t("pages:modelAccess.title")} note={t("pages:modelAccess.note")}>
      {!v.principal_state && <Notice className="mb-3" data-model-access-shared>{t("pages:modelAccess.sharedState")}</Notice>}
      {v.harnesses.length ? <Panel>{v.harnesses.map((h) => (
        <div key={h.id} data-model-access-harness={h.id}>
          <Row icon={<HarnessMark harness={h.id} className="size-5" />} title={h.name}
            sub={h.base_url ? <span className="type-code break-all">{h.base_url}</span> : t("pages:modelAccess.notSet")}
            onClick={v.can_edit ? () => setOpen(open === h.id ? null : h.id) : undefined}>
            {h.problem ? <Pill tone="attention" data-model-access-problem={h.problem}><TriangleAlertIcon className="size-3" />{t(`pages:modelAccess.problem.${h.problem}`)}</Pill>
              : !h.base_url && !h.required ? <Pill data-model-access-optional>{t("pages:modelAccess.optional")}</Pill>
              : <Pill tone="success" data-model-access-ok>{t("pages:modelAccess.ok")}</Pill>}
          </Row>
          {open === h.id && <Editor h={h} items={v.items} onSaved={(x) => { setV(x); setOpen(null) }} />}
        </div>
      ))}</Panel> : <p className="type-callout text-muted-foreground">{t("pages:modelAccess.none")}</p>}
      {!v.can_edit && <p className="mt-2 type-footnote text-muted-foreground" data-model-access-readonly>{t("pages:modelAccess.ownerOnly")}</p>}
      <div className="mt-3"><BlockedList rows={v.blocked} /></div>
    </Section>
  )
}

function Editor({ h, items, onSaved }: { h: AccessHarness; items: AccessItem[]; onSaved: (v: ModelAccess) => void }) {
  const t = useT()
  const [url, setUrl] = useState(h.base_url ?? "")
  const [item, setItem] = useState(h.item ?? items[0]?.id ?? "")
  const fields = items.find((i) => i.id === item)?.fields ?? []
  const [field, setField] = useState(h.field ?? fields[0] ?? "")
  const [busy, setBusy] = useState(false)
  const run = async (f: () => Promise<ModelAccess>, ok: string) => {
    setBusy(true)
    try { onSaved(await f()); toast(ok) } catch (e) { toast(e instanceof Error ? e.message : String(e)) } finally { setBusy(false) }
  }
  const pickItem = (id: string) => { setItem(id); setField(items.find((i) => i.id === id)?.fields[0] ?? "") }
  return (
    <div className="flex flex-col gap-3 px-3 pb-4 pt-1" data-model-access-editor={h.id}>
      <label className="flex flex-col gap-1.5"><span className="type-label text-muted-foreground">{t("pages:modelAccess.baseUrl")}</span>
        <Input value={url} placeholder="https://" onChange={(e) => setUrl(e.currentTarget.value)} data-model-access-url /></label>
      {items.length ? <>
        <div className="flex flex-col gap-1.5"><span className="type-label text-muted-foreground">{t("pages:modelAccess.item")}</span>
          <SetSelect wide value={item} testId="model-access-item" onChange={pickItem} options={items.map((i) => ({ v: i.id, l: i.title, description: t(`pages:modelAccess.scope.${i.scope}`) }))} /></div>
        <div className="flex flex-col gap-1.5"><span className="type-label text-muted-foreground">{t("pages:modelAccess.field")}</span>
          <SetSelect wide value={field} testId="model-access-field" onChange={setField} options={fields.map((f) => ({ v: f, l: f }))} /></div>
      </> : <Notice tone="attention" data-model-access-no-items>{t("pages:modelAccess.noItems")} <a href="#/vault" className="text-blue hover:underline">{t("pages:modelAccess.fix.vault")}</a></Notice>}
      <p className="type-footnote text-muted-foreground">{t("pages:modelAccess.keyNote")}</p>
      <div className="flex flex-wrap justify-end gap-2">
        {h.base_url && <Button size="sm" variant="ghost" className="text-destructive" disabled={busy} onClick={() => void run(() => modelAccessApi.clear(h.id), t("pages:modelAccess.removed", { name: h.name }))} data-model-access-clear>{t("pages:modelAccess.remove")}</Button>}
        <Button size="sm" disabled={busy || !url.trim() || !item || !field} onClick={() => void run(() => modelAccessApi.put(h.id, { base_url: url.trim(), item, field }), t("pages:modelAccess.saved", { name: h.name }))} data-model-access-save>{busy && <Spinner />}{t("common:action.save")}</Button>
      </div>
    </div>
  )
}
