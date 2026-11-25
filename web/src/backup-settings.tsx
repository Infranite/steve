import { useEffect, useState } from "react"
import { HubError } from "@/api"
import { systemApi } from "@/api/system"
import { Row, Panel, Section } from "@/ui"
import { useT } from "@/i18n"
import { monthDayTime } from "@/i18n/format"
import { bytes } from "@/disk"
import { SetSelect, SetSwitch } from "@/settings-controls"
import { lastKey, planKey, type BackupSettings, type BackupStatus } from "@/backup"

/** Settings · 数据 · Hub 上的数据: where this Hub keeps its database and the copies of it, as the Hub says (an
 * installation's directory is its own), and when the last daily copy was made, a row each (a row's second line is cut after two lines). The Hub answers the instance owner
 * only (paths of its machine); anyone else sees nothing here. */
export function HubData() {
  const [s, setS] = useState<BackupStatus | null>(null)
  const [err, setErr] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const [hidden, setHidden] = useState(false), t = useT()
  useEffect(() => {
    systemApi.backup().then(setS)
      .catch((e) => { if (e instanceof HubError && e.status === 403) setHidden(true); else setErr(e instanceof Error ? e.message : String(e)) })
  }, [])
  const save = async (patch: Partial<BackupSettings>) => {
    if (!s || saving) return
    setSaving(true); setErr(null)
    try {
      const settings = await systemApi.saveBackup({ ...s.settings, ...patch })
      setS({ ...s, settings, daily: s.available && settings.enabled, keep_days: settings.keep_days })
    } catch (e) { setErr(e instanceof Error ? e.message : String(e)) }
    finally { setSaving(false) }
  }
  if (hidden) return null
  const last = s && lastKey(s)
  return <Section data-hub-data label={t("manage:data.hub")}><Panel>
    <Row title={t("manage:data.location")} sub={<span className="type-code break-all" data-hub-database>{s?.database ?? err ?? "…"}</span>} />
    {s && <fieldset disabled={saving} className="min-w-0" data-backup-settings>
      <Row title={t("manage:data.backup")} sub={<span data-hub-backup={s.daily ? "daily" : "off"}>{t(`manage:data.${planKey(s)}`, { days: s.keep_days })}{!s.available && ` ${t("manage:data.backupUnavailable")}`}</span>}>
        <SetSwitch label={t("manage:data.backup")} testId="backup-daily" checked={s.settings.enabled} onChange={(enabled) => void save({ enabled })} />
      </Row>
      <Row title={t("manage:data.backupKeepDays")} sub={t("manage:data.backupKeepDaysSub")}>
        <SetSelect label={t("manage:data.backupKeepDays")} testId="backup-keep-days" disabled={saving} value={String(s.settings.keep_days)}
          options={[...new Set([1, 3, 7, 14, 30, 90, 365, s.settings.keep_days])].sort((a, b) => a - b).map((days): [string, string] => [String(days), t("common:dur.days", { count: days })])}
          onChange={(days) => void save({ keep_days: Number(days) })} />
      </Row>
    </fieldset>}
    {err && <Row title={<span role="alert">{err}</span>} />}
    {s && <Row title={t("manage:data.backupDir")} sub={<span className="type-code break-all" data-hub-backup-dir>{s.dir}</span>} />}
    {s && last && <Row title={t("manage:data.backupLatest")} sub={<span data-hub-backup-last>{t(`manage:data.${last}`, s.last ? { when: monthDayTime(s.last.at), size: bytes(s.last.bytes) } : {})}</span>} />}
  </Panel></Section>
}
