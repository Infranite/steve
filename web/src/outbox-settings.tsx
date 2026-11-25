import { useEffect, useState } from "react"
import { HubError } from "@/api"
import { systemApi } from "@/api/system"
import { Button } from "@/components/ui/button"
import { Row, Panel, Section, Notice } from "@/ui"
import { toast } from "@/store"
import { useT } from "@/i18n"
import { deadSub, reviveText, type OutboxReport } from "@/outbox"

/** Settings · 数据 · 后台投递: the outbox's dead deliveries (given up on after a rejection or a day of failures),
 * one row per consumer, each sent again on request. Shown only when there are some, and only to the instance
 * owner (the Hub answers 403 to anyone else). */
export function OutboxDead() {
  const [r, setR] = useState<OutboxReport | null>(null), [error, setError] = useState("")
  const [busy, setBusy] = useState<string | null>(null), t = useT()
  const load = () => systemApi.outbox()
    .then((v) => { setR(v); setError("") })
    .catch((e: Error) => { if (e instanceof HubError && e.status === 403) { setR(null); setError("") } else setError(e.message) })
  useEffect(() => { void load() }, [])
  if (!error && !r?.groups.length) return null
  const revive = async (consumer: string) => {
    setBusy(consumer)
    try {
      const x = await systemApi.reviveOutbox(consumer)
      toast(reviveText(x.revived))
      await load()
    } catch (e) { toast(e instanceof Error ? e.message : String(e)) }
    setBusy(null)
  }
  return <Section data-outbox-dead label={t("shell:outbox.title")} note={t("shell:outbox.note")}>
    {error && <Notice role="alert" tone="destructive" action={<Button size="xs" variant="outline" onClick={() => void load()}>{t("common:action.retry")}</Button>}>{error}</Notice>}
    {!!r?.groups.length && <Panel>
      {r.groups.map((g) => <Row key={g.consumer} data-outbox-group={g.consumer}
        title={<span className="type-code">{g.consumer}{g.topic && g.topic !== g.consumer && <span className="ml-1.5 text-muted-foreground">{g.topic}</span>}</span>}
        sub={<span className="break-words">{deadSub(g)}</span>}>
        <Button size="xs" variant="outline" disabled={busy !== null} onClick={() => void revive(g.consumer)} data-outbox-revive={g.consumer}>{busy === g.consumer ? t("shell:outbox.reviving") : t("shell:outbox.revive")}</Button>
      </Row>)}
    </Panel>}
  </Section>
}
