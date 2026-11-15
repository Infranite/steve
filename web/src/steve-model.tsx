/* Model choice stays available before any model runs and beside failed replies. Configuration is
 * one atomic request; listing an adapter/model never claims that its provider has remaining quota. */
import { useEffect, useRef, useState } from "react"
import { Button } from "@/components/ui/button"
import { Checkbox } from "@/components/ui/checkbox"
import { Input } from "@/components/ui/input"
import { ModelPicker } from "@/components/affect/model-picker-clean"
import { HarnessMark, ModelMark } from "@/components/harness-mark"
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from "@/components/ui/sheet"
import { modReq } from "@/api"
import { ModelPicker as CatalogPicker, useCatalog, harnessName } from "@/model-picker"
import { chatBusy, refresh, st, useStore } from "@/store"
import { Select } from "@/view-shared"
import { useT } from "@/i18n"
import { Loading, Reveal, Turn } from "@/ui"
import { CpuIcon } from "lucide-react"

type Choice = { harness: string; models: string[]; effort?: string | null }
type View = {
  steve: { harness: string; model: string | null; models: string[] }
  lightweight?: Choice | null
  lightweight_effective?: Choice
  bot_revision: number
  harnesses: { id: string; name: string; ready: boolean | null; judgments: boolean }[]
  purposes: { key: string; label: string; role?: "primary" | "lightweight"; follows: boolean; effective: { harness: string; models: string[] } }[]
}
export const modelSetupApi = {
  get: () => modReq<View>("GET", "/settings/models"),
  select: (choice: { harness: string; model: string | null; expected_revision: number; follow_purposes: boolean; lightweight?: Choice | null }) => modReq<View>("POST", "/settings/models/select", choice),
}

export function SteveModelForm({ first = false, onSaved, onDirty, afterSave }: { first?: boolean; onSaved?: () => void; onDirty?: () => void; afterSave?: () => Promise<unknown> }) {
  useStore()
  const t = useT(), ws = st.workspace?.id
  const [view, setView] = useState<View | null>(null), [error, setError] = useState("")
  const [harness, setHarness] = useState(""), [model, setModel] = useState("__unselected")
  const [follow, setFollow] = useState(true), [busy, setBusy] = useState(false), [saved, setSaved] = useState(false)
  const [manual, setManual] = useState(false)
  const [preview, setPreview] = useState(false), [advanced, setAdvanced] = useState(false)
  const [lightOn, setLightOn] = useState(false), [lightHarness, setLightHarness] = useState(""), [lightModel, setLightModel] = useState("")
  const alive = useRef(true), inFlight = useRef(false)
  const cat = useCatalog(harness)
  const load = () => {
    setError("")
    modelSetupApi.get().then((v) => {
      if (!alive.current || st.workspace?.id !== ws) return
      setView(v)
      setLightOn(!!v.lightweight); setLightHarness(v.lightweight?.harness ?? ""); setLightModel(v.lightweight?.models[0] ?? "")
      if (!first) { setHarness(v.steve.harness); setModel(v.steve.model ?? "__default") }
    }, (e) => alive.current && setError(String(e.message ?? e)))
  }
  useEffect(() => { alive.current = true; load(); return () => { alive.current = false } }, [])
  const dirty = () => { setSaved(false); onDirty?.() }
  const save = async () => {
    if (!view || !harness || model === "__unselected" || lightOn && !lightHarness || inFlight.current || st.workspace?.id !== ws) return
    inFlight.current = true; setBusy(true); setError("")
    try {
      const v = await modelSetupApi.select({ harness, model: model === "__default" ? null : model, expected_revision: view.bot_revision, follow_purposes: follow, lightweight: lightOn ? view.lightweight?.harness === lightHarness && (view.lightweight.models[0] ?? "") === lightModel ? view.lightweight : { harness: lightHarness, models: lightModel ? [lightModel] : [] } : null })
      if (!alive.current || st.workspace?.id !== ws) return
      setView(v); setSaved(true); onSaved?.()
      // Refresh failure must not make an already-committed selection look unsaved.
      void refresh().catch(() => {})
      if (afterSave) await afterSave()
    } catch (e) { if (alive.current && st.workspace?.id === ws) setError(e instanceof Error ? e.message : String(e)) }
    finally { inFlight.current = false; if (alive.current) setBusy(false) }
  }
  if (!view) return <div className="flex flex-col gap-3">{error ? <><p role="alert" className="type-callout text-destructive">{error}</p><Button type="button" variant="outline" onClick={load}>{t("common:action.retry")}</Button></> : <Loading />}</div>
  const overrides = view.purposes.filter((p) => !p.follows)
  const available = view.harnesses.filter((h) => h.ready !== false && h.judgments)
  const current = view.steve.models.join(" → ") || t("pages:models.setup.adapterDefault")
  const choices = [{ id: "__default", name: cat?.default ? t("pages:models.setup.defaultIs", { model: cat.default }) : t("pages:models.setup.adapterDefault"), description: t("pages:models.setup.quotaUnknown"), icon: <HarnessMark harness={harness} /> }, ...(cat?.models ?? []).map((m) => ({ id: m.id, name: m.label || m.id, description: m.note ?? m.id, icon: <ModelMark model={m.id} /> }))]
  if (model !== "__unselected" && model !== "__default" && !choices.some((m) => m.id === model)) choices.push({ id: model, name: model, description: t("pages:models.setup.custom"), icon: <ModelMark model={model} /> })
  return <div className="flex min-w-0 flex-col gap-4" data-steve-model-form>
    <p className="type-callout text-muted-foreground" data-current-model>{t("pages:models.setup.current", { harness: view.harnesses.find((h) => h.id === view.steve.harness)?.name ?? harnessName(view.steve.harness), model: current })}</p>
    <div className="flex flex-col gap-2 type-callout"><span>{t("pages:models.setup.harness")}</span>
      <Select value={harness} label={t("pages:models.setup.harness")} placeholder={t("pages:models.setup.chooseHarness")} disabled={busy} testId="steve-harness" options={available.map((h) => ({ v: h.id, l: h.name }))} onChange={(h) => { dirty(); setHarness(h); setModel("__unselected"); setManual(false) }} />
    </div>
    <p className="type-footnote text-muted-foreground">{t("pages:models.roles.primaryUses")}</p>
    {harness && <div className="flex min-w-0 flex-col gap-2" data-steve-model-picker>
      <span className="type-callout">{t("pages:models.setup.model")}</span>
      <fieldset disabled={busy} className="min-w-0">
        <ModelPicker key={harness} models={choices} value={model} showSettings={false} onChange={(m) => { dirty(); setModel(m); setManual(false) }} labels={{ model: t("pages:models.setup.model"), choose: t("pages:models.setup.chooseModel"), settings: t("pages:models.setup.title"), none: t("pages:models.setup.noModels") }} />
      </fieldset>
      <Button type="button" size="xs" variant="ghost" className="self-start text-muted-foreground" disabled={busy} onClick={() => setManual(!manual)} data-steve-model-manual>{t("pages:models.setup.custom")}</Button>
      {manual && <Input aria-label={t("pages:models.setup.modelId")} placeholder={t("pages:models.setup.modelId")} disabled={busy} value={model.startsWith("__") ? "" : model} onChange={(e) => { dirty(); setModel(e.target.value.trim() || "__unselected") }} />}
    </div>}
    <div className="border-t pt-4" data-lightweight-config>
      <Checkbox label={t("pages:models.roles.enableLight")} disabled={busy} checked={lightOn} onCheckedChange={(v) => { dirty(); setLightOn(v); if (v && !lightHarness) setLightHarness(harness) }} data-lightweight-enable />
      <p className="mt-2 type-footnote text-muted-foreground">{t("pages:models.roles.lightUses")}</p>
      {lightOn ? <div className="mt-3 flex flex-col gap-3">
        <Select value={lightHarness} label={t("pages:models.roles.lightHarness")} placeholder={t("pages:models.setup.chooseHarness")} disabled={busy} testId="lightweight-harness" options={available.map((h) => ({ v: h.id, l: h.name }))} onChange={(h) => { dirty(); setLightHarness(h); setLightModel("") }} />
        {lightHarness && <fieldset disabled={busy} data-lightweight-picker><CatalogPicker harness={lightHarness} value={lightModel} onChange={(m) => { dirty(); setLightModel(m) }} allowDefault testId="lightweight-model" /></fieldset>}
      </div> : <p className="mt-2 type-callout text-muted-foreground">{t("pages:models.roles.lightFollows")}</p>}
    </div>
    <p className="type-footnote text-muted-foreground">{t("pages:models.setup.quotaUnknown")}</p>
    <div><button type="button" className="flex items-center gap-2 type-callout text-muted-foreground" aria-expanded={advanced} onClick={() => setAdvanced(!advanced)} data-role-advanced-toggle><Turn open={advanced} />{t("pages:models.roles.advanced")}</button><Reveal open={advanced}><div className="pt-3">    <Checkbox label={t("pages:models.setup.follow")} disabled={busy} checked={follow} onCheckedChange={(v) => { dirty(); setFollow(v === true) }} data-steve-follow /></div></Reveal></div>
    {overrides.length > 0 && <p className="type-footnote text-muted-foreground" data-model-overrides>{t(follow ? "pages:models.setup.clearOverrides" : "pages:models.setup.keepOverrides", { purposes: overrides.map((p) => p.label).join(t("pages:listSep")) })}</p>}
    <p className="type-footnote text-muted-foreground">{t("pages:models.setup.scope")}</p>
    {!available.length && <p role="status" className="type-callout text-attention">{t("pages:models.setup.noHarness")}</p>}
    <div data-role-preview><button type="button" className="flex items-center gap-2 type-callout text-muted-foreground" aria-expanded={preview} onClick={() => setPreview(!preview)} data-role-preview-toggle><Turn open={preview} />{t("pages:models.roles.preview", { count: view.purposes.length })}</button><Reveal open={preview}>
      <ul className="mt-2 flex flex-col divide-y divide-border">{view.purposes.map((p) => {
        const light = lightOn && p.role === "lightweight"
        const chosen = !follow && !p.follows ? p.effective : light ? { harness: lightHarness, models: lightModel ? [lightModel] : [] } : { harness, models: model.startsWith("__") ? [] : [model] }
        return <li key={p.key} className="flex flex-wrap justify-between gap-1 py-2 type-footnote" data-role-preview-purpose={p.key}><span>{p.label}</span><span className="break-all text-muted-foreground">{chosen.harness ? harnessName(chosen.harness) : t("pages:models.setup.chooseHarness")} · {chosen.models.join(" → ") || t("pages:models.setup.adapterDefault")}</span></li>
      })}</ul></Reveal>
    </div>
    {error && <div role="alert" className="flex flex-col gap-2 type-callout text-destructive" data-steve-model-error>{error}<Button type="button" variant="outline" size="sm" className="self-start" onClick={load} disabled={busy}>{t("pages:models.setup.reload")}</Button></div>}
    <div className="flex flex-wrap items-center gap-3">
      <Button type="button" disabled={busy || !available.some((h) => h.id === harness) || lightOn && !available.some((h) => h.id === lightHarness) || model === "__unselected" || saved && !afterSave} onClick={() => void save()} data-steve-model-save>{busy ? t("pages:models.setup.saving") : afterSave ? t("pages:models.setup.saveRetry") : t("pages:models.setup.save")}</Button>
      {saved && <span role="status" className="type-footnote text-success" data-steve-model-saved>{t("pages:models.setup.saved")}</span>}
    </div>
    {afterSave && <p className="type-footnote text-muted-foreground">{t("pages:models.setup.retryHint")}</p>}
  </div>
}

/** Owners can reach this even when setup or a provider is failing. No model call until explicit retry. */
export function SteveModelControl({ retry, label }: { retry?: () => Promise<unknown>; label?: string }) {
  useStore()
  const t = useT(), [open, setOpen] = useState(false), ws = st.workspace?.id
  const bot = st.bots.steve
  useEffect(() => setOpen(false), [ws])
  if (!st.owner) return null
  return <>
    <Button type="button" size="sm" variant={retry || label ? "outline" : "ghost"} className="max-w-full text-muted-foreground" onClick={() => setOpen(true)} data-steve-model-control={retry ? "retry" : label ? "settings" : "chat"}><CpuIcon /><span className="truncate">{label ?? (retry ? t("pages:models.setup.changeRetry") : `${harnessName(bot?.harness ?? "")} · ${bot?.model ?? t("pages:models.setup.adapterDefault")}`)}</span></Button>
    <Sheet open={open} onOpenChange={setOpen}><SheetContent className="w-full! sm:max-w-[480px]!" data-steve-model-sheet>
      <SheetHeader><SheetTitle>{t("pages:models.setup.title")}</SheetTitle><SheetDescription>{t("pages:models.setup.description")}</SheetDescription></SheetHeader>
      <div className="min-h-0 flex-1 overflow-y-auto px-6 pb-6"><SteveModelForm key={ws} afterSave={retry ? async () => { if (st.workspace?.id !== ws || chatBusy()) throw new Error(t("pages:models.setup.busy")); await retry(); setOpen(false) } : undefined} /></div>
    </SheetContent></Sheet>
  </>
}
