import { HarnessMark, ModelMark } from "@/components/harness-mark"
import { useT } from "@/i18n"
import { botName, type Sess, useDeviceInventory } from "@/store"
import { Status } from "@/ui"
import { ActorAvatar } from "@/components/actor-avatar"

export function SessionIdentity({ session, compact = false }: { session: Sess; compact?: boolean }) {
  const t = useT()
  const harness = session.h ?? t("core:unknownHarness")
  const model = session.m ?? t("core:unknownModel")
  const devices = useDeviceInventory(!!session.node)
  const node = session.node ? devices.find((d) => d.node_id === session.node || d.id === session.node)?.name ?? session.node : t("core:unknownNode")
  const bot = session.b ? botName(session.b) : t("core:unknownBot")
  return (
    <span className="inline-flex min-w-0 items-center gap-1.5" data-execution-identity
      title={[bot, harness, model, node].join(" · ")}>
      {session.b && <ActorAvatar actorId={session.b} size={16} />}
      <span className={compact ? "sr-only" : "truncate"}>{bot}</span>
      <HarnessMark harness={session.harness} title={harness} className="size-3.5" />
      <span className={compact ? "sr-only" : "truncate"}>{harness}</span>
      <ModelMark model={session.m} className="size-3.5" />
      <span className={compact ? "sr-only" : "truncate"}>{model}</span>
      <span className={compact ? "sr-only" : "truncate"}>{node}</span>
      {!compact && <Status s={session.st} sess />}
    </span>
  )
}
