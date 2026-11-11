/** Settings · 数据 · Hub 上的数据: the Hub's `/api/system/backup`: where the database and its copies are on the
 * Hub's machine, whether the daily copy is on, and the newest one. */
export type BackupStatus = { database: string; dir: string; daily: boolean; keep_days: number; last: { name: string; at: number; bytes: number } | null }

/** what the backup row says of the plan, and the row of the newest daily copy says (no such row when the daily
 *  copy is off and none is left): keys under `manage:data` */
export const planKey = (s: BackupStatus): "backupDaily" | "backupOff" => (s.daily ? "backupDaily" : "backupOff")
export const lastKey = (s: BackupStatus): "backupLast" | "backupNone" | null => (s.last ? "backupLast" : s.daily ? "backupNone" : null)
