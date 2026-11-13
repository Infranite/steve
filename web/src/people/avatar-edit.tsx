/* A contact's picture on its page, and the way to change it: without one, a click picks an image; with one, a menu
 * offers another or going back to initials. The Hub cuts the centre square and keeps 256px (crate::avatar), so
 * any PNG, JPEG or WebP up to 10 MB will do; a wrong type or size is said here before anything is sent. */
import { useRef, useState } from "react"
import { cn } from "cn"
import { CameraIcon } from "lucide-react"
import { Spinner } from "@/components/ui/spinner"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { toast } from "@/store"
import { useT } from "@/i18n"
import { avatarSrc, people, type Contact } from "@/api/people"
import { ContactAvatar } from "@/people/avatar"
import { act } from "@/people/common"

const TYPES = ["image/png", "image/jpeg", "image/webp"]
const MAX_MB = 10

export function AvatarEdit({ c, size = 64, onChanged }: { c: Contact; size?: number; onChanged?: (c: Contact) => void }) {
  const t = useT()
  const input = useRef<HTMLInputElement>(null)
  const [busy, setBusy] = useState(false)
  const has = !!avatarSrc(c.avatar), unconfirmed = c.kind === "unconfirmed"
  const pick = () => input.current?.click()
  const upload = async (f: File) => {
    if (!TYPES.includes(f.type)) return toast(t("people:avatar.wrongType"))
    if (f.size > MAX_MB * 1024 * 1024) return toast(t("people:avatar.tooLarge", { mb: MAX_MB }))
    setBusy(true)
    const r = await act(people.setAvatar(c.id, f), t("people:avatar.saved"))
    setBusy(false)
    if (r) onChanged?.(r)
  }
  const clear = async () => { setBusy(true); const r = await act(people.clearAvatar(c.id), t("people:avatar.removed")); setBusy(false); if (r) onChanged?.(r) }
  const label = has ? t("people:avatar.change") : t("people:avatar.add")
  const face = (
    <>
      <ContactAvatar name={c.name} avatar={c.avatar} unconfirmed={unconfirmed} size={size} />
      {/* a camera over the picture on hover / focus; without a picture a small one on its corner says it can have one */}
      <span aria-hidden className={cn("absolute inset-0 flex items-center justify-center rounded-full bg-black/45 text-white opacity-0 transition-opacity duration-120 ease-(--ease-out) group-hover/av:opacity-100 group-focus-visible/av:opacity-100", busy && "opacity-100")}>
        {busy ? <Spinner className="size-5" /> : <CameraIcon className="size-5" strokeWidth={1.75} />}
      </span>
      {!has && !busy && <span aria-hidden className="absolute -right-0.5 -bottom-0.5 flex size-6 items-center justify-center rounded-full bg-card text-muted-foreground shadow-[var(--lift)] transition-opacity duration-120 group-hover/av:opacity-0"><CameraIcon className="size-3.5" strokeWidth={1.75} /></span>}
    </>
  )
  const btn = "group/av relative block shrink-0 rounded-full outline-none focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:cursor-progress"
  return (
    <>
      <input ref={input} type="file" accept={TYPES.join(",")} className="hidden" data-avatar-input
        onChange={(e) => { const f = e.target.files?.[0]; e.target.value = ""; if (f) void upload(f) }} />
      {has ? (
        <DropdownMenu>
          <DropdownMenuTrigger render={<button type="button" className={btn} aria-label={label} disabled={busy} data-avatar-edit="menu" />}>{face}</DropdownMenuTrigger>
          <DropdownMenuContent align="start" className="min-w-40">
            <DropdownMenuItem onClick={pick} data-avatar-change>{t("people:avatar.change")}</DropdownMenuItem>
            <DropdownMenuSeparator />
            <DropdownMenuItem variant="destructive" onClick={() => void clear()} data-avatar-remove>{t("people:avatar.remove")}</DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      ) : (
        <button type="button" className={btn} aria-label={label} title={t("people:avatar.hint")} disabled={busy} onClick={pick} data-avatar-edit="pick">{face}</button>
      )}
    </>
  )
}
