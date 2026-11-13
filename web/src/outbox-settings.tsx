import { useEffect, useState } from "react"
import { HubError, modReq } from "@/api"
import { Button } from "@/components/ui/button"
import { Row, Panel, Section } from "@/ui"
import { toast } from "@/store"
import { useT } from "@/i18n"
import { deadSub, reviveText, type OutboxReport } from "@/outbox"

/** Settings · 数据 · 后台投递: the outbox's dead deliveries (given up on after a rejection or a day of failures),
 * one row per consumer, each sent again on request. Shown only when there are some, and only to the instance
 * owner (the Hub answers 403 to anyone else). */
export function OutboxDead() {
  const [r, setR] = useState<OutboxReport | null>(null)
  const [busy, setBusy] = useState<string | null>(null), t = useT()
  const load = () => modReq<OutboxReport>("GET", "/system/outbox")
    .then(setR)
    .catch((e) => { if (!(e instanceof HubError && (e.status === 403 || e.status === 404))) toast(e instanceof Error ? e.message : String(e)) })
  useEffect(() => { void load() }, [])
  if (!r || r.groups.length === 0) return null
  const revive = async (consumer: string) => {
    setBusy(consumer)
    try {
      const x = await modReq<{ revived: number }>("POST", "/system/outbox/revive", { consumer })
      toast(reviveText(x.revived))
      await load()
    } catch (e) { toast(e instanceof Error ? e.message : String(e)) }
    setBusy(null)
  }
  return <Section data-outbox-dead label={t("shell:outbox.title")} note={t("shell:outbox.note")}>
    <Panel>
      {r.groups.map((g) => <Row key={g.consumer} data-outbox-group={g.consumer}
        title={<span className="type-code">{g.consumer}{g.topic && g.topic !== g.consumer && <span className="ml-1.5 text-muted-foreground">{g.topic}</span>}</span>}
        sub={<span className="break-words">{deadSub(g)}</span>}>
        <Button size="xs" variant="outline" disabled={busy !== null} onClick={() => void revive(g.consumer)} data-outbox-revive={g.consumer}>{busy === g.consumer ? t("shell:outbox.reviving") : t("shell:outbox.revive")}</Button>
      </Row>)}
    </Panel>
  </Section>
}
