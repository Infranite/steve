/* Who is signed in, as the sidebar's account row and a phone's 我的 button. On a phone the tab bar holds 对话 · 待处理 ·
 * 日历 · 待办 · 事项, and 我的 (every other place, 信息流 first) is the avatar at the top left of each top-level page. */
import { useEffect, useState } from "react"
import { UserIcon } from "lucide-react"
import { cn } from "cn"
import { api } from "@/api"
import { useMyAvatar } from "@/api/people"
import { useT } from "@/i18n"
import { st } from "@/store"

export type Me = { name: string; email: string | null; role: string }
/* GET /me once per page: the name and email do not change with the workspace (the role does, and comes from the
 * store's workspaces); a fixture or a Hub that does not answer leaves it unknown */
let asked: Promise<Me | null> | null = null
/** who is signed in, null until known */
export function useMe() {
  const [me, setMe] = useState<Me | null>(null)
  useEffect(() => {
    let on = true
    asked ??= api.me().then((r) => r.user, () => { asked = null; return null })
    void asked.then((u) => { if (on) setMe(u) })
    return () => { on = false }
  }, [])
  return me
}
const initial = (name: string) => (Array.from(name.trim())[0] ?? "").toUpperCase()
/** your picture (uploaded on 我) or your initial */
export function Initials({ name, src, className }: { name: string; src?: string; className?: string }) {
  const c = initial(name)
  if (src) return <img src={src} alt="" aria-hidden className={cn("shrink-0 rounded-full object-cover", className)} data-account-avatar />
  return <span aria-hidden className={cn("flex shrink-0 items-center justify-center rounded-full bg-primary/12 font-semibold text-primary", className)}>{c || <UserIcon className="size-3.5" strokeWidth={1.75} />}</span>
}

/** a phone's way to 我的: your avatar, with a dot when a newer Steve can be installed. Not on 我的 itself, nor wider
 *  than a phone, where the sidebar's account row does this */
export function MineButton({ className }: { className?: string }) {
  const t = useT(), me = useMe(), pic = useMyAvatar()
  if (location.hash.replace(/^#\/?/, "").split(/[/?]/)[0] === "more") return null
  return (
    <a href="#/more" aria-label={t("core:nav.mine")} className={cn("relative flex size-8 shrink-0 items-center justify-center rounded-full md:hidden", className)} data-mine>
      <Initials name={me?.name ?? ""} src={pic} className="size-7 type-label leading-none" />
      {st.update && <span className="absolute -right-0.5 -top-0.5 size-2 rounded-full bg-attention ring-2 ring-background" data-mine-update-dot />}
    </a>
  )
}
