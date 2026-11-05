/* Shared pieces of the channel pages (domain-model.md §1.4): a space's face (a group icon and member count, the other
 * party's avatar for a DM, “你自己” for your chat with yourself), the attention quick menu (实时 · 不提醒 · 屏蔽) and the
 * notes for settings that are on but do not apply here. Generic: nothing here knows a provider. */
import type { ReactNode } from "react"
import { cn } from "cn"
import { BanIcon, BellOffIcon, CheckIcon, EllipsisIcon, InfoIcon, TriangleAlertIcon, UserIcon, UsersIcon } from "lucide-react"
import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { ContactAvatar } from "@/people/avatar"
import { ATTENTION, SPACE_KIND, attentionOf, isDm, spaceTitle, type Attention, type Conflict, type Space } from "@/api/channels"
import { t, useT } from "@/i18n"

/** group: a group icon; DM: the other party's initials; your chat with yourself: a person */
export function SpaceAvatar({ s, size = 32 }: { s: Space; size?: number }) {
  if (s.self_chat || s.kind === "self_dm") return <span className="inline-flex shrink-0 items-center justify-center rounded-full bg-blue/12 text-blue" style={{ width: size, height: size }} aria-hidden><UserIcon style={{ width: size * 0.55, height: size * 0.55 }} strokeWidth={1.75} /></span>
  if (isDm(s)) return <ContactAvatar name={s.counterpart?.name || spaceTitle(s)} size={size} />
  return <span className="inline-flex shrink-0 items-center justify-center rounded-[28%] bg-muted text-muted-foreground" style={{ width: size, height: size }} aria-hidden><UsersIcon style={{ width: size * 0.5, height: size * 0.5 }} strokeWidth={1.5} /></span>
}

/** a name that never pushes the layout sideways: one line, ellipsis, full name on hover */
export function Name({ children, className }: { children: string; className?: string }) {
  return <span className={cn("block min-w-0 truncate", className)} title={children}>{children}</span>
}

/** “群 · 12 人” / “私聊” / “和自己的对话” */
export function kindLine(s: Space) {
  if (s.self_chat || s.kind === "self_dm") return SPACE_KIND.self_dm
  if (isDm(s)) return s.counterpart?.is_bot ? t("channels:bits.botDm") : SPACE_KIND.dm
  return [SPACE_KIND[s.kind] ?? t("channels:bits.group"), s.member_count ? t("channels:bits.members", { count: s.member_count }) : ""].filter(Boolean).join(" · ")
}

const ATT_ICON: Record<Attention, ReactNode> = { realtime: null, quiet: <BellOffIcon className="size-3.5" aria-hidden />, blocked: <BanIcon className="size-3.5" aria-hidden /> }
/** a small label for 不提醒 / 屏蔽 (nothing for 实时) */
export function AttentionTag({ a }: { a: Attention }) {
  if (a === "realtime") return null
  return <span className="inline-flex shrink-0 items-center gap-1 type-footnote text-muted-foreground" data-attention-tag={a}>{ATT_ICON[a]}{ATTENTION[a][0]}</span>
}

/** the quick action on a list row: hover on desktop, always there on touch */
export function AttentionMenu({ s, onPick, disabled }: { s: Space; onPick: (a: Attention) => void; disabled?: boolean }) {
  const t = useT()
  const cur = attentionOf(s)
  return (
    <DropdownMenu>
      <DropdownMenuTrigger disabled={disabled} render={<button type="button" aria-label={t("channels:bits.attentionAria", { name: spaceTitle(s) })} className="flex size-8 shrink-0 items-center justify-center rounded-lg text-muted-foreground opacity-0 transition-opacity hover:bg-accent hover:text-foreground focus-visible:opacity-100 group-hover/space:opacity-100 data-[popup-open]:opacity-100 [@media(hover:none)]:opacity-100" data-space-menu={s.id} />}>
        <EllipsisIcon className="size-4" /></DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="w-60">
        <DropdownMenuGroup><DropdownMenuLabel className="type-footnote text-muted-foreground">{t("channels:bits.disturb")}</DropdownMenuLabel>
        {(Object.keys(ATTENTION) as Attention[]).map((a) => (
          <DropdownMenuItem key={a} onClick={() => a !== cur && onPick(a)} data-attention-pick={a} className="items-start">
            <span className="mt-0.5 size-4 shrink-0">{a === cur && <CheckIcon className="size-4" />}</span>
            <span className="min-w-0"><span className="block">{ATTENTION[a][0]}</span><span className="block type-footnote text-muted-foreground">{ATTENTION[a][1]}</span></span>
          </DropdownMenuItem>))}</DropdownMenuGroup>
        <DropdownMenuSeparator />
        <DropdownMenuItem onClick={() => { location.hash = "#/channels/space/" + s.id }}>
          <span className="size-4 shrink-0" />{t("channels:bits.openSettings")}</DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

/** settings that are on but do not apply here, next to the setting (conflict) or what still applies (info) */
export function Conflicts({ items, keys, className }: { items?: Conflict[]; keys: string[]; className?: string }) {
  const xs = (items ?? []).filter((c) => keys.includes(c.key))
  if (!xs.length) return null
  return (
    <div className={cn("mt-2 flex flex-col gap-1", className)}>
      {xs.map((c, n) => (
        <div key={n} className={cn("flex items-start gap-1.5 rounded-lg px-3 py-2 type-footnote", c.level === "conflict" ? "bg-attention/10 text-foreground" : "bg-muted/60 text-muted-foreground")} data-conflict={c.key} data-conflict-level={c.level}>
          {c.level === "conflict" ? <TriangleAlertIcon className="mt-px size-3.5 shrink-0 text-attention" aria-hidden /> : <InfoIcon className="mt-px size-3.5 shrink-0" aria-hidden />}
          <span className="min-w-0 break-words">{c.text}</span>
        </div>))}
    </div>
  )
}
