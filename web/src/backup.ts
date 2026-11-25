import type { SnapshotId } from "./gen"

/** Settings · 数据 · Hub 上的数据: the Hub's `/api/system/backup`: where the database and its copies are on the
 * Hub's machine, whether the daily copy is on, and the newest one. */
export type BackupSettings = { enabled: boolean; keep_days: number }
export type BackupStatus = {
  settings: BackupSettings
  available: boolean
  database: string
  dir: string
  last: { snapshot_id: SnapshotId; at: number } | null
  repository: { bytes: number; available_bytes: number } | null
  error: string | null
}

const daily = (s: BackupStatus) => s.available && s.settings.enabled
export const planKey = (s: BackupStatus) => daily(s) ? "backupDaily" : "backupOff"
export const lastKey = (s: BackupStatus) => s.last ? "backupLast" : daily(s) ? "backupNone" : null

/** The owner-only backup endpoint saves both installation settings atomically. */
export async function saveBackupSettings(
  settings: BackupSettings,
  request: (method: "PATCH", path: string, body: BackupSettings) => Promise<BackupSettings>,
): Promise<BackupSettings> {
  if (!Number.isInteger(settings.keep_days) || settings.keep_days < 1 || settings.keep_days > 365) throw new Error("Invalid backup retention")
  return request("PATCH", "/system/backup", settings)
}
