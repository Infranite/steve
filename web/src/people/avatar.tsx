/* contacts, organizations and teams get affectUI's Avatar (components/ui/avatar): a soft color of their own from the
 * name, tuned for both themes – initials for a contact, a building on a rounded square for an organization, a group
 * icon for a team; one not yet confirmed a dashed muted shape. A contact with a picture of its own (`avatar.file`,
 * uploaded on its page) shows it, the initials while it loads or when it fails.
 * No @/ui imports, so the composer's @ menu can use it without an import cycle */
import { cn } from "cn"
import { BuildingIcon, UsersIcon } from "lucide-react"
import { Avatar } from "@/components/ui/avatar"
import { avatarSrc } from "@/api/people"

export function ContactAvatar({ name, type = "contact", unconfirmed, size = 28, avatar, className }: { name: string; type?: "contact" | "org" | "team"; unconfirmed?: boolean; size?: number; avatar?: unknown; className?: string }) {
  const icon = type === "org" ? <BuildingIcon strokeWidth={1.5} /> : type === "team" ? <UsersIcon strokeWidth={1.5} /> : undefined
  return <Avatar name={name || "?"} src={type === "contact" ? avatarSrc(avatar) : undefined} size={size} shape={type === "contact" ? "circle" : "square"} icon={icon} aria-label="" className={cn("shrink-0", className)}
    data-unconfirmed={unconfirmed || undefined} data-contact-avatar data-contact-kind={type} />
}
