/* How the Hub hands a Node work: whether it takes new work, how many sessions it runs at once, and its tags.
 * A paired Node is set through the Node registry (pause, resume, limit). The Hub's own Node (and a device row that
 * names no Node) through its device row, written with its revision: a change made elsewhere meanwhile is refused,
 * said in place with "use the latest", never written over. Pausing holds only new sessions; a lower limit never cuts a
 * running turn. Tags let flows and Bots send work to a Node that carries one. */
import { useEffect, useState } from "react"
import { ActivityIcon } from "lucide-react"
import { api, SESSIONS_MAX, type RDevice } from "@/api"
import { Button } from "@/components/ui/button"
import { Switch } from "@/components/ui/switch"
import { Spinner } from "@/components/ui/spinner"
import { NumberField } from "@/components/ui/pickers"
import { TagInput } from "@/components/affect/tag-input-clean"
import { refresh, toast } from "@/store"
import { useT } from "@/i18n"
import type { NodeView } from "@/device-model"
import { useDevices } from "@/devices-data"
import { DCard, DRow } from "@/devices-ui"

/** the device row's persisted status words (the Hub stores them as they are) */
const PAUSED = "暂停接活", NORMAL = "正常"
/** a tag as the Hub keeps it (steve-store `tag_of`): letters, digits, `-`, `_`, `.` and `:`, at most 40 */
const TAG = /^[\p{L}\p{N}_.:-]{1,40}$/u

/** one way to set what the Hub hands this Node: through the registry, or through its device row and revision */
export function useWorkSettings(v: NodeView) {
  const t = useT(), { reloadNodes } = useDevices()
  const [busy, setBusy] = useState(false), [conflict, setConflict] = useState(false)
  const viaDevice = (v.local || v.legacy) && !!v.device
  const editable = viaDevice ? v.device!.can_configure === true && v.device!.revision != null : !!v.node && (v.state === "active" || v.state === "paused")
  const write = async (f: () => Promise<unknown>, done: string) => {
    setBusy(true); setConflict(false)
    try { await f(); toast(done); reloadNodes(); if (viaDevice) void refresh() }
    catch (e) { if (viaDevice && String(e).includes("device_revision_conflict")) setConflict(true); else toast((e as Error).message) }
    finally { setBusy(false) }
  }
  const patch = (d: RDevice, p: { status?: string; max_sessions?: number }) => api.patchDevice(d.id, { revision: d.revision!, ...p })
  return {
    editable, busy, conflict,
    /** reads the latest values instead of the draft that met a change made elsewhere */
    useLatest: () => { setConflict(false); reloadNodes() },
    setReceive: (on: boolean) => write(() => viaDevice ? patch(v.device!, { status: on ? NORMAL : PAUSED }) : on ? api.resumeNode(v.id) : api.pauseNode(v.id), t(on ? "devices:node.resumed" : "devices:node.paused")),
    setLimit: (n: number) => write(() => viaDevice ? patch(v.device!, { max_sessions: n }) : api.nodeLimit(v.id, n), t("shell:devices.limitSaved", { count: n })),
  }
}

export function TakeWork({ v, w }: { v: NodeView; w: ReturnType<typeof useWorkSettings> }) {
  const t = useT()
  const [n, setN] = useState<number | null>(v.max ?? null)
  useEffect(() => setN(v.max ?? null), [v.max])
  const valid = n != null && Number.isInteger(n) && n >= 1 && n <= SESSIONS_MAX, dirty = valid && n !== v.max
  return (
    <DCard icon={<ActivityIcon />} title={t("devices:work.rules")} sub={t("devices:work.rulesSub")} data-node-take-work>
      <div className="border-t border-border/70">
        {w.conflict && <div className="flex flex-wrap items-center gap-2 bg-attention/10 px-5 py-2.5 type-callout text-attention" role="alert" data-config-conflict>
          <span className="min-w-0 flex-1">{t("devices:work.conflict")}</span><Button size="xs" variant="outline" onClick={w.useLatest} data-config-latest>{t("devices:work.useLatest")}</Button></div>}
        <DRow title={t("devices:node.receive")} sub={t("devices:node.receiveSub")}>
          {w.busy && <Spinner className="size-3.5 text-muted-foreground" />}
          <Switch checked={!v.paused} disabled={!w.editable || w.busy} onCheckedChange={(on) => void w.setReceive(on)} aria-label={t("devices:node.receive")} data-node-receive={v.paused ? "off" : "on"} />
        </DRow>
        {v.max != null && <DRow title={t("devices:node.limit")} sub={t("devices:node.limitSub")} className="flex-wrap" data-session-limit={v.max}>
          {w.editable ? <form className="flex shrink-0 items-center gap-2" onSubmit={(e) => { e.preventDefault(); if (dirty && n != null) void w.setLimit(n) }}>
            <NumberField size="sm" min={1} max={SESSIONS_MAX} step={1} value={n} onValueChange={setN} error={!valid ? t("shell:devices.limitInvalid", { max: SESSIONS_MAX }) : undefined} disabled={w.busy} aria-label={t("devices:node.limit")} className="w-28" data-session-limit-input />
            {dirty && <Button size="xs" type="submit" disabled={w.busy} data-session-limit-save>{t("common:action.save")}</Button>}
          </form> : <span className="type-callout tabular-nums text-muted-foreground">{v.max}</span>}
        </DRow>}
        {v.max != null && <p className="px-5 pb-3 -mt-1 type-footnote text-muted-foreground" data-limit-semantics>{t("shell:devices.limitEffect")}</p>}
        {!v.legacy && <NodeTags node={v.id} />}
      </div>
    </DCard>
  )
}

/** the tags a flow or a Bot can send work by (B7), on any Node, the Hub's own included */
function NodeTags({ node }: { node: string }) {
  const t = useT()
  const [tags, setTags] = useState<string[] | null>(null), [error, setError] = useState("")
  useEffect(() => {
    let live = true
    void api.nodeTags().then(r => { if (live) setTags(r.tags[node] ?? []) }).catch(() => { if (live) setTags([]) })
    return () => { live = false }
  }, [node])
  if (tags === null) return null
  const save = async (next: string[]) => {
    setError(""); const prev = tags; setTags(next)
    try { setTags((await api.setNodeTags(node, next)).tags) } catch (e) { setTags(prev); setError((e as Error).message) }
  }
  return (
    <div className="flex flex-col gap-2 border-t border-border/70 px-5 py-3" data-node-tags>
      <div><div className="type-body text-foreground">{t("shell:devices.tags.title")}</div><div className="type-footnote text-muted-foreground">{t("shell:devices.tags.sub")}</div></div>
      <TagInput value={tags} onChange={(x) => void save(x)} placeholder={t("shell:devices.tags.add")} aria-label={t("shell:devices.tags.title")} max={32} splitOnSpace
        normalize={(x) => x.trim().toLowerCase()}
        labels={{ more: t("devices:work.tagMore"), remove: (tag) => t("shell:devices.tags.remove", { tag }), max: (n) => t("devices:work.tagMax", { n }), added: (xs) => t("devices:work.tagAdded", { tags: xs.join(", ") }) }} validate={(x) => (TAG.test(x) ? true : t("devices:work.tagBad"))} error={error || undefined} data-node-tag-input />
    </div>
  )
}
