/* 项目页的「合并队列」（STEVE_MQ=1，取代合并通道）：目标的合入方式和授权、排队和在途的批次（二分时的两半挂在原批次下）、
 * 发布列车和它什么时候关闭、近 7 天的统计，以及每个数字的设置（空着用默认）。 */
import { api, type RQueue, type RQueueBatch, type RQueuePolicy } from "@/api"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Textarea } from "@/components/ui/textarea"
import { toast } from "@/store"
import { errMsg } from "@/github/index"
import { Group, Meta, Row, SectionLabel } from "@/ui"
import { useEffect, useState } from "react"
import { useT } from "@/i18n"
import { POLICY_FIELDS, ordered, patchOf, shown } from "@/merge-queue-model"

const clock = (ms: number) => new Date(ms).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })
const mins = (ms: number | null | undefined) => (ms == null ? "–" : String(Math.round(ms / 60_000)))

function BatchRow({ b, depth }: { b: RQueueBatch; depth: number }) {
  const t = useT()
  const names = [...b.members.map((m) => m.label), ...(b.others ? [t("shell:mergeQueue.others", { count: b.others })] : [])].join(t("shell:list.sep"))
  const state = t(`shell:mergeQueue.batchState.${b.state}` as "shell:mergeQueue.batchState.running", { defaultValue: b.state })
  const step = b.state === "running" && b.step ? ` · ${t(`shell:mergeQueue.step.${b.step}` as "shell:mergeQueue.step.integrating", { defaultValue: b.step })}` : ""
  return <Row className={depth ? "pl-6" : ""} title={`MQ-${b.no} · ${state}${step}`} sub={[names, b.reruns ? t("shell:mergeQueue.reruns", { count: b.reruns }) : ""].filter(Boolean).join(" · ")} data-queue-batch={b.no} />
}

export function QueueSection({ project, queue, onQueue }: { project: string; queue: RQueue; onQueue: (q: RQueue | null) => void }) {
  const t = useT()
  const [busy, setBusy] = useState(false)
  const [inputs, setInputs] = useState<Partial<Record<keyof RQueuePolicy, string>>>({})
  useEffect(() => setInputs({}), [queue.set])
  const run = async (act: Parameters<typeof api.queueAct>[1], done?: string) => {
    setBusy(true)
    try { const r = await api.queueAct(project, act); onQueue(r.queue); if (done) toast(done) } catch (e) { toast(errMsg(e)) } finally { setBusy(false) }
  }
  const save = () => {
    const p = patchOf(queue.set, inputs)
    if (!p) { toast(t("shell:mergeQueue.policy.bad")); return }
    if (!Object.keys(p).length) return
    void run({ policy: p as never }, t("common:state.saved"))
  }
  const tg = queue.target
  const mode = tg?.mode ? t(`shell:mergeQueue.mode.${tg.mode}` as "shell:mergeQueue.mode.push") : t("shell:mergeQueue.mode.unknown")
  const open = queue.trains?.find((r) => r.state === "open")
  const flight = queue.trains?.find((r) => ["awaiting", "cutting", "failed", "unknown", "halted"].includes(r.state))
  const s = queue.stats
  const queued = queue.queued ?? []
  return (
    <section data-queue>
      <SectionLabel>{t("shell:mergeQueue.title")}</SectionLabel>
      <Group>
        {!queue.on && <Row title={t("shell:mergeQueue.noNode")} data-queue-off />}
        <Row title={t("shell:mergeQueue.modeTitle", { mode })} data-queue-target
          sub={tg ? [tg.migrated ? t("shell:mergeQueue.migrated") : "", tg.refused ? t("shell:mergeQueue.refused") : tg.granted ? t("shell:mergeQueue.granted") : t("shell:mergeQueue.notGranted")].filter(Boolean).join(" · ") : t("shell:mergeQueue.nothingYet")}>
          {(!tg || !tg.granted) && <span className="flex gap-1.5">
            <Button size="sm" variant="outline" disabled={busy} onClick={() => void run({ allow: false })} data-queue-deny>{t("shell:mergeQueue.deny")}</Button>
            <Button size="sm" disabled={busy} onClick={() => void run({ allow: true }, t("shell:mergeQueue.allowed"))} data-queue-allow>{t("shell:mergeQueue.allow")}</Button>
          </span>}
        </Row>
        {queue.policy.paused && <Row title={t("shell:mergeQueue.paused")} data-queue-paused>
          <Button size="sm" variant="outline" disabled={busy} onClick={() => void run({ once: true })} data-queue-once>{t("shell:mergeQueue.once")}</Button>
        </Row>}
        {(queued.length > 0 || (queue.queued_others ?? 0) > 0) && <Row title={t("shell:mergeQueue.queued", { count: queued.length + (queue.queued_others ?? 0) })} sub={queued.map((e) => e.label).join(t("shell:list.sep"))} data-queue-queued />}
        {ordered(queue.batches ?? []).slice(0, 12).map(({ b, depth }) => <BatchRow key={b.no} b={b} depth={depth} />)}
        {!queue.batches?.length && !queued.length && <Row title={t("shell:mergeQueue.idle")} data-queue-idle />}
      </Group>
      <div className="mt-3">
        <Group>
          {flight && <Row title={t("shell:mergeQueue.train", { no: flight.no, state: t(`shell:mergeQueue.trainState.${flight.state}` as "shell:mergeQueue.trainState.awaiting", { defaultValue: flight.state }) })}
            sub={flight.members.map((m) => m.label).join(t("shell:list.sep"))} data-queue-flight />}
          {open && open.members.length + open.others > 0
            ? <Row title={t("shell:mergeQueue.trainOpen", { no: open.no, count: open.members.length + open.others, time: open.closes_at ? clock(open.closes_at) : "–" })} sub={open.members.map((m) => m.label).join(t("shell:list.sep"))} data-queue-train>
              <Button size="sm" variant="outline" disabled={busy || !!flight} onClick={() => void run({ release_now: true }, t("shell:mergeQueue.releasing"))} data-queue-release-now>{t("shell:mergeQueue.releaseNow")}</Button>
            </Row>
            : !flight && <Row title={t("shell:mergeQueue.noTrain")} data-queue-no-train />}
        </Group>
      </div>
      {s && <div className="mt-3"><Group>
        <Row title={t("shell:mergeQueue.stats.title")} data-queue-stats sub={<Meta>
          <span>{t("shell:mergeQueue.stats.batches", { count: s.batches, members: s.avg_members ?? "–" })}</span>
          <span>{t("shell:mergeQueue.stats.wait", { minutes: mins(s.avg_wait_to_land_ms) })}</span>
          <span>{t("shell:mergeQueue.stats.outcomes", { landed: s.landed, failed: s.failed, unknown: s.no_result })}</span>
          <span>{t("shell:mergeQueue.stats.returns", { bisections: s.bisections, conflicts: s.returned_conflict, failed: s.returned_failed, reruns: s.reruns })}</span>
          <span>{t("shell:mergeQueue.stats.trains", { count: s.trains, size: s.avg_train_size ?? "–", minutes: mins(s.avg_train_ms) })}</span>
          <span>{t("shell:mergeQueue.stats.cards", { count: s.cards, yolo: s.cards_by_yolo, minutes: mins(s.avg_answer_ms) })}</span>
          <span>{t("shell:mergeQueue.stats.batchTime", { minutes: mins(s.avg_batch_ms) })}</span>
        </Meta>} />
      </Group></div>}
      <div className="mt-3"><Group>
        {POLICY_FIELDS.map((f) => {
          const value = inputs[f.key] ?? shown(f, queue.set[f.key])
          const placeholder = shown(f, queue.defaults[f.key])
          const label = t(`shell:mergeQueue.policy.${f.key}` as "shell:mergeQueue.policy.max_batch")
          return <Row key={f.key} title={label} sub={f.kind === "minutes" || f.kind === "optionalMinutes" ? t("shell:mergeQueue.policy.inMinutes") : undefined} data-queue-policy={f.key}>
            {f.kind === "list"
              ? <Textarea className="min-h-16 w-64 font-mono text-[12px]" value={value} placeholder={t("shell:mergeQueue.policy.noChecks")} onChange={(e) => setInputs({ ...inputs, [f.key]: e.target.value })} aria-label={label} />
              : f.kind === "switch"
                ? <Badge variant="outline" className="cursor-pointer select-none" onClick={() => setInputs({ ...inputs, [f.key]: value ? "" : "1" })}>{value ? t("shell:mergeQueue.policy.on") : t("shell:mergeQueue.policy.off")}</Badge>
                : <Input className="h-8 w-20 text-right tabular-nums" inputMode="decimal" value={value} placeholder={placeholder} onChange={(e) => setInputs({ ...inputs, [f.key]: e.target.value })} aria-label={label} />}
          </Row>
        })}
        <Row title={t("shell:mergeQueue.policy.defaultsNote")} data-queue-save>
          <Button size="sm" variant="ghost" disabled={busy || !Object.keys(inputs).length} onClick={save}>{t("common:action.save")}</Button>
        </Row>
      </Group></div>
    </section>
  )
}
