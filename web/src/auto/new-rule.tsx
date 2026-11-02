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

const TRIGGER_TITLE: Record<string, string> = { mail: "邮件 · 收到新邮件", cal: "日程 · 日程提醒", im: "IM · 收到消息", github: "GitHub · 仓库事件", webhook: "告警 · 收到告警", time: "定时 · 按时间", item: "事项 · 事项状态变化", manual: "手动 · 你点运行" }
const blank = (source: string): Graph => ({ nodes: [{ id: "t", kind: "trigger", title: TRIGGER_TITLE[source] ?? "触发", note: "", config: source === "time" ? { source, cron: "0 9 * * 1-5" } : { source, types: [] } }], edges: [] })

export function NewRuleDialog({ source: pre, onClose }: { source?: string; onClose: () => void }) {
  const [how, setHow] = useState<"steve" | "paste">("steve")
  const [text, setText] = useState(""), [src, setSrc] = useState(pre ?? "mail"), [busy, setBusy] = useState(false)
  const run = async (f: () => Promise<{ id: string }>, ok: string) => {
    setBusy(true)
    try { const r = await f(); onClose(); go("#/auto/rule/" + r.id); toast(ok) } catch (e) { toast(errText(e)) } finally { setBusy(false) }
  }
  const paste = () => {
    let g: any
    try { g = JSON.parse(text) } catch { toast("不是合法的 JSON"); return }
    const graph: Graph = g?.graph ?? g
    if (!Array.isArray(graph?.nodes)) { toast("JSON 里要有 nodes 和 edges"); return }
    void run(() => flowApi.newFlow({ name: String(g?.name ?? "粘贴的流程").slice(0, 40), description: g?.description, graph: { nodes: graph.nodes, edges: graph.edges ?? [] } }), "已建流程，默认停用")
  }
  return (
    <Modal title="新建流程" desc="用一句话描述，Steve 起草成流程图；也可以从空白开始自己画，或者粘贴流程的文本形式（JSON）。" onClose={onClose}
      footer={how === "steve"
        ? <><Button variant="ghost" disabled={busy} onClick={() => void run(() => flowApi.newFlow({ name: "新流程", graph: blank(src) }), "已建空白流程")} data-rule-blank>从空白开始</Button>
            <Button disabled={busy || text.trim().length < 2} onClick={() => void run(() => flowApi.draftFlow(src, text.trim()), "Steve 起草好了，检查后启用")} data-testid="rule-go"><SparklesIcon />{busy ? "Steve 在起草…" : "让 Steve 起草"}</Button></>
        : <><Button variant="outline" onClick={onClose}>取消</Button><Button disabled={busy || !text.trim()} onClick={paste} data-rule-paste>新建</Button></>}>
      <Segmented size="sm" value={how} onChange={(v) => { setHow(v); setText("") }} options={[["steve", "描述"], ["paste", "粘贴文本形式"]] as const} />
      <FieldGroup className="gap-4">
        {how === "steve" ? <>
          <Field orientation="horizontal" className="justify-between"><FieldLabel>事件源</FieldLabel><Pick label="事件源" value={src} options={Object.entries(SOURCE_LABEL).map(([v, l]) => ({ v, l }))} onChange={setSrc} testId="rule-src" /></Field>
          <Field><FieldLabel htmlFor="rule-desc">要它做什么</FieldLabel><Textarea id="rule-desc" autoFocus maxLength={2000} className="min-h-24 rounded-lg" placeholder="例如：带发票的邮件，500 元以下的交通和餐饮直接提交报销，其余等我审批" value={text} onChange={(e) => setText(e.target.value)} data-rule-desc /></Field>
        </> : <Field><FieldLabel htmlFor="rule-json">流程的 JSON</FieldLabel><Textarea id="rule-json" autoFocus className="min-h-48 rounded-lg bg-muted/50 font-mono text-[12px]" placeholder={'{"name": "…", "graph": {"nodes": […], "edges": […]}}'} value={text} onChange={(e) => setText(e.target.value)} data-rule-json />
          <FieldDescription>可以是流程页“代码”里复制的 JSON，或者带 name 的 {"{name, graph}"}。新建后默认停用。</FieldDescription></Field>}
      </FieldGroup>
    </Modal>
  )
}
