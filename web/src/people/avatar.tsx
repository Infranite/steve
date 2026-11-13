/* contacts get calm initials on a muted tint of their colour (readable in both themes, never black on navy), organizations a building, teams a group icon (prototype ContactAvatar); no ui imports, so the
 * composer's @ menu can use it without an import cycle */
import { cn } from "cn"
import { BuildingIcon, UsersIcon } from "lucide-react"
import { defaultColor } from "@/store"

export function ContactAvatar({ name, type = "contact", unconfirmed, size = 28, className }: { name: string; type?: "contact" | "org" | "team"; unconfirmed?: boolean; size?: number; className?: string }) {
  const square = size >= 48 ? "rounded-xl" : size >= 24 ? "rounded-lg" : "rounded-md"
  if (type === "team") return <span className={cn("inline-flex shrink-0 items-center justify-center bg-muted text-muted-foreground", square, className)} style={{ width: size, height: size }} aria-hidden><UsersIcon style={{ width: size * 0.5, height: size * 0.5 }} strokeWidth={1.5} /></span>
  const t = /^[A-Za-z]/.test(name) ? name.split(/\s+/).map((w) => w[0]).slice(0, 2).join("").toUpperCase() : name.slice(0, 1)
  return (
    <span className={cn("inline-flex shrink-0 select-none items-center justify-center font-medium text-foreground/70", type === "org" ? square : "rounded-full", unconfirmed && "border border-dashed border-foreground/25 !bg-muted text-muted-foreground", className)}
      style={{ width: size, height: size, background: `color-mix(in oklab, ${defaultColor(name || "?")} 22%, var(--muted))`, fontSize: size * 0.4 }} aria-hidden>
      {type === "org" ? <BuildingIcon style={{ width: size * 0.5, height: size * 0.5 }} strokeWidth={1.5} /> : t}
    </span>
  )
}

