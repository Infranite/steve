/* 新建流程 (prototype NewRuleDialog): Steve drafts it from a sentence, or start blank, or paste the JSON form */
import { useState } from "react"
import { SparklesIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Textarea } from "@/components/ui/textarea"
import { Field, FieldDescription, FieldGroup, FieldLabel } from "@/components/ui/field"
import { Segmented, go } from "@/ui"
import { toast } from "@/store"
import { flowApi, errText, SOURCE_LABEL, type Graph } from "@/api/flow"
import { Modal, Pick } from "@/auto/common"
import { t as tr, useT } from "@/i18n"

const TRIGGER_TITLE = ["mail", "cal", "im", "github", "webhook", "time", "item", "manual"] as const
const triggerTitle = (s: string) => tr(`auto:triggerTitle.${(TRIGGER_TITLE as readonly string[]).includes(s) ? (s as (typeof TRIGGER_TITLE)[number]) : "other"}`)
const blank = (source: string): Graph => ({ nodes: [{ id: "t", kind: "trigger", title: triggerTitle(source), note: "", config: source === "time" ? { source, cron: "0 9 * * 1-5" } : { source, types: [] } }], edges: [] })

export function NewRuleDialog({ source: pre, onClose }: { source?: string; onClose: () => void }) {
  const [how, setHow] = useState<"steve" | "paste">("steve")
  const [text, setText] = useState(""), [src, setSrc] = useState(pre ?? "mail"), [busy, setBusy] = useState(false)
  const t = useT()
  const run = async (f: () => Promise<{ id: string }>, ok: string) => {
    setBusy(true)
    try { const r = await f(); onClose(); go("#/auto/rule/" + r.id); toast(ok) } catch (e) { toast(errText(e)) } finally { setBusy(false) }
  }
  const paste = () => {
    let g: any
    try { g = JSON.parse(text) } catch { toast(t("auto:notJson")); return }
    const graph: Graph = g?.graph ?? g
    if (!Array.isArray(graph?.nodes)) { toast(t("auto:newRule.jsonNeedsNodes")); return }
    void run(() => flowApi.newFlow({ name: String(g?.name ?? t("auto:newRule.pastedName")).slice(0, 40), description: g?.description, graph: { nodes: graph.nodes, edges: graph.edges ?? [] } }), t("auto:newRule.createdOff"))
  }
  return (
    <Modal title={t("auto:newFlow")} desc={t("auto:newRule.desc")} onClose={onClose}
      footer={how === "steve"
        ? <><Button variant="ghost" disabled={busy} onClick={() => void run(() => flowApi.newFlow({ name: t("auto:newRule.blankName"), graph: blank(src) }), t("auto:newRule.createdBlank"))} data-rule-blank>{t("auto:newRule.fromBlank")}</Button>
            <Button disabled={busy || text.trim().length < 2} onClick={() => void run(() => flowApi.draftFlow(src, text.trim()), t("auto:newRule.drafted"))} data-testid="rule-go"><SparklesIcon />{busy ? t("auto:newRule.drafting") : t("auto:newRule.draft")}</Button></>
        : <><Button variant="outline" onClick={onClose}>{t("common:action.cancel")}</Button><Button disabled={busy || !text.trim()} onClick={paste} data-rule-paste>{t("auto:newRule.create")}</Button></>}>
      <Segmented size="sm" value={how} onChange={(v) => { setHow(v); setText("") }} options={[["steve", t("auto:newRule.howSteve")], ["paste", t("auto:newRule.howPaste")]] as const} />
      <FieldGroup className="gap-4">
        {how === "steve" ? <>
          <Field orientation="horizontal" className="justify-between"><FieldLabel>{t("auto:newRule.source")}</FieldLabel><Pick label={t("auto:newRule.source")} value={src} options={Object.entries(SOURCE_LABEL).map(([v, l]) => ({ v, l }))} onChange={setSrc} testId="rule-src" /></Field>
          <Field><FieldLabel htmlFor="rule-desc">{t("auto:newRule.what")}</FieldLabel><Textarea id="rule-desc" autoFocus maxLength={2000} className="min-h-24 rounded-lg" placeholder={t("auto:newRule.whatPlaceholder")} value={text} onChange={(e) => setText(e.target.value)} data-rule-desc /></Field>
        </> : <Field><FieldLabel htmlFor="rule-json">{t("auto:newRule.json")}</FieldLabel><Textarea id="rule-json" autoFocus className="min-h-48 rounded-lg bg-muted/50 font-mono text-[12px]" placeholder={'{"name": "…", "graph": {"nodes": […], "edges": […]}}'} value={text} onChange={(e) => setText(e.target.value)} data-rule-json />
          <FieldDescription>{t("auto:newRule.jsonHint")}</FieldDescription></Field>}
      </FieldGroup>
    </Modal>
  )
}
