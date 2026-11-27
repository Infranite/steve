/* Owner system administration, health, audit, disk, backup, outbox, and web-perf endpoints. */
import { modReq } from "@/api"
import type { AuditPage } from "@/audit"
import { saveBackupSettings, type BackupSettings, type BackupStatus } from "@/backup"
import type { DiskReport, Reclaimed } from "@/disk"
import type { HealthWindow, HubHealth } from "@/health"
import type { OutboxReport } from "@/outbox"

export const systemApi = {
  audit: (limit: number, before?: number) =>
    modReq<AuditPage>("GET", `/audit?limit=${limit}${before ? `&before=${before}` : ""}`),
  workspaces: () =>
    modReq<DiskReport>("GET", "/system/workspaces", undefined, AbortSignal.timeout(120_000)),
  reclaimWorkspaces: () =>
    modReq<Reclaimed>("POST", "/system/workspaces/reclaim", {}, AbortSignal.timeout(600_000)),
  backup: () => modReq<BackupStatus>("GET", "/system/backup"),
  saveBackup: (settings: BackupSettings) =>
    saveBackupSettings(settings, (method, path, body) => modReq<BackupSettings>(method, path, body)),
  health: (window: HealthWindow = "15m") =>
    modReq<HubHealth>("GET", `/system/health?window=${encodeURIComponent(window)}`),
  outbox: () => modReq<OutboxReport>("GET", "/system/outbox"),
  reviveOutbox: (consumer: string) =>
    modReq<{ revived: number }>("POST", "/system/outbox/revive", { consumer }),
  webPerf: (report: unknown) => modReq("POST", "/system/web-perf", report),
}
