/* Personal, caller/workspace-scoped notifications. Shapes mirror steve-channels/notifications.rs;
 * these are not workspace notices or provider read receipts. */
import { modReq } from "@/api"
import { currentHub } from "@/hub"
import type { Contact } from "@/api/people"

export type NotificationPolicy = {
  enabled: boolean; mode: "now" | "digest" | "log"; party_id: string | null; space_id: string | null
  timezone: string; quiet_start: number | null; quiet_end: number | null
}
export type PersonalNotification = {
  id: string; event_id: string; mode: string; body: string | null; state: string
  delivery_id: string | null; seen_at: number | null; created: number
}
export type NotificationTarget = { party_id: string; space_id: string; party_name: string; space_name: string; channel: string }
export const notificationsApi = {
  forWorkspace(workspace: string) {
    // Capture both: the global request workspace changes before the old form leaves the screen,
    // and a Hub chooser can change the destination while a resource is still mounted.
    const scope = { workspace, hub: currentHub() }
    const req = <T>(method: string, path: string, body?: unknown) => modReq<T>(method, path, body, undefined, scope)
    return {
      me: () => req<Contact>("GET", "/me/contact"),
      targets: () => req<{ targets: NotificationTarget[] }>("GET", "/me/notification-targets"),
      policy: () => req<NotificationPolicy>("GET", "/me/notification-policy"),
      save: (policy: NotificationPolicy) => req<{ ok: true }>("PUT", "/me/notification-policy", policy),
      inbox: () => req<PersonalNotification[]>("GET", "/me/notifications"),
      seen: (id: string) => req<{ ok: true }>("POST", `/me/notifications/${encodeURIComponent(id)}/seen`, {}),
    }
  },
}
export type NotificationsClient = ReturnType<typeof notificationsApi.forWorkspace>

export const minuteTime = (minute: number | null) => minute == null ? null : `${String(Math.floor(minute / 60)).padStart(2, "0")}:${String(minute % 60).padStart(2, "0")}`
export const timeMinute = (time: string | null) => {
  if (!time || !/^([01]\d|2[0-3]):[0-5]\d$/.test(time)) return null
  const [hours, minutes] = time.split(":").map(Number)
  return hours * 60 + minutes
}
export const validQuiet = (start: number | null, end: number | null) =>
  (start === null && end === null) || (start !== null && end !== null && Number.isInteger(start) && Number.isInteger(end) && start >= 0 && start < 1440 && end >= 0 && end < 1440 && start !== end)
export const validTimezone = (zone: string) => {
  if (!zone.trim() || /^[+-]/.test(zone)) return false
  try { new Intl.DateTimeFormat("en", { timeZone: zone }).format(); return true } catch { return false }
}
