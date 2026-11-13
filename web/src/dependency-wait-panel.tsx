import { useState } from "react"
import { api, type DependencyWait } from "@/api"
import { Button } from "@/components/ui/button"
import { Textarea } from "@/components/ui/textarea"
import { useT } from "@/i18n"
import { loadItem } from "@/store"
import { Select } from "@/view-shared"

export function DependencyWaitPanel({ item, wait }: { item: string; wait: DependencyWait }) {
  const t = useT()
  const [resume, setResume] = useState(wait.resume_override)
  const [reason, setReason] = useState(wait.reason ?? "")
  const [busy, setBusy] = useState(false), [error, setError] = useState("")
  if (wait.phase !== "active" || !wait.open_dependencies.length && !wait.waiting) return null
  const save = async () => {
    setBusy(true); setError("")
    try {
      await api.saveDependencyWait(item, { expected_cycle: wait.cycle, resume, reason: reason.trim() })
      await loadItem(item, true)
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
      await loadItem(item, true)
    } finally { setBusy(false) }
  }
  return <section className="min-w-0 rounded-xl bg-muted/60 p-4" data-dependency-wait>
    <h2 className="type-headline">{t("views:dependencyWait.title")}</h2>
    <p className="mt-1.5 type-callout" role="status" data-wait-policy>{wait.waiting
      ? t(wait.resume_mode === "automatic" ? "views:dependencyWait.savedAutomatic" : "views:dependencyWait.savedNotify")
      : t("views:dependencyWait.inherited")}</p>
    {!wait.waiting && <p className="mt-1 type-footnote text-muted-foreground">{t("views:dependencyWait.policy", { mode: t(wait.resume_mode === "automatic" ? "views:dependencyWait.automatic" : "views:dependencyWait.notify") })}</p>}
    {!wait.resume_enabled && <p className="mt-1 type-callout text-attention" data-wait-blocker>{t("views:dependencyWait.schedulingHeld", { reason: wait.resume_blocker })}</p>}
    {wait.resume_fallback_reason && <p className="mt-1 type-callout">{t("views:dependencyWait.fallback", { reason: wait.resume_fallback_reason })}</p>}
    <p className="mt-1 type-footnote text-muted-foreground">{t("views:dependencyWait.safety")}</p>
    {wait.active_turn && <p className="mt-1 type-footnote text-muted-foreground">{t("views:dependencyWait.active")}</p>}
    <div className="mt-4 flex flex-col items-start gap-3">
      <Select value={resume} onChange={(v) => setResume(v as DependencyWait["resume_override"])} testId="dependency-resume"
        options={(["workspace", "automatic", "notify"] as const).map((v) => ({ v, l: t(`views:dependencyWait.${v}`) }))} />
      <label className="flex w-full flex-col gap-1.5 type-label text-muted-foreground">
        {t("views:dependencyWait.reason")}
        <Textarea value={reason} onChange={(e) => setReason(e.target.value)} maxLength={2000} placeholder={t("views:dependencyWait.reasonPlaceholder")} className="min-h-16 bg-background type-callout text-foreground" data-wait-reason />
      </label>
      <Button size="sm" variant="outline" disabled={busy || !wait.can_register || !reason.trim()} onClick={() => void save()} data-wait-save>{t(busy ? "views:dependencyWait.saving" : "views:dependencyWait.save")}</Button>
      {!wait.can_register && wait.register_blocker && <p className="type-footnote text-muted-foreground">{wait.register_blocker}</p>}
      {error && <p role="alert" className="type-callout text-destructive">{error}</p>}
    </div>
  </section>
}
