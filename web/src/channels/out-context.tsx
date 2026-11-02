/* What an outbound message answers, above its draft on the approval card: the message it replies to
 * (with the few before it, folded), where it goes and as whom. Hidden when the Hub has no such route. */
import { useEffect, useState } from "react"
import { outContext, type ROutContext } from "@/api/channels"

const SPACE_KIND: Record<string, string> = { group: "群", topic_group: "话题群", channel: "话题群", dm: "私聊", self_dm: "你自己" }

export function OutContext({ id }: { id?: string }) {
  const [c, setC] = useState<ROutContext | null>(null)
  const [open, setOpen] = useState(false)
  useEffect(() => { if (!id) return; let live = true; outContext(id).then((x) => live && setC(x ?? null), () => live && setC(null)); return () => { live = false } }, [id])
  if (!c) return null
  const who = c.as_you ? "以你本人发出（开头带 [AI]）" : c.persona ? `以「${c.persona}」发出` : "以机器人发出"
  return (
    <div className="mb-3 flex flex-col gap-2 rounded-lg border bg-muted/30 px-3 py-2.5 type-callout" data-out-context>
      <div className="flex flex-wrap items-center gap-x-1.5 type-footnote text-muted-foreground">
        <button type="button" className="font-medium text-foreground/80 hover:underline" onClick={() => { location.hash = "#/channels/space/" + c.space.id }}>{c.space.name}</button>
        <span>{SPACE_KIND[c.space.kind] ?? c.space.kind}</span>
        {c.space.external && <span className="text-attention">有外部成员</span>}
        <span className="text-tertiary">·</span><span>{who}</span>
      </div>
      {c.reply_to ? <>
        {c.before.length > 0 && (open
          ? <div className="flex flex-col gap-1 border-l-2 pl-2 type-footnote text-muted-foreground" data-out-before>{c.before.map((b, n) => <div key={n} className="break-words"><span className="font-medium">{b.sender}：</span>{b.text ?? "（已清理）"}</div>)}</div>
          : <button type="button" className="self-start type-footnote text-blue hover:underline" onClick={() => setOpen(true)}>看前面 {c.before.length} 条</button>)}
        <div className="break-words" data-out-reply-to><span className="font-medium">{c.reply_to.sender}：</span>{c.reply_to.text ?? "（已清理）"}</div>
        <div className="type-footnote text-muted-foreground">下面是要回复给 {c.reply_to.sender} 的内容</div>
      </> : <div className="type-footnote text-muted-foreground">直接发到这个{SPACE_KIND[c.space.kind] ?? "聊天"}，不是回复某一条消息</div>}
    </div>
  )
}
