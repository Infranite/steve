import { useEffect, useState } from "react"
import { HubError, modReq } from "@/api"
import { Row, Panel, Section } from "@/ui"
import { useT } from "@/i18n"
import { monthDayTime } from "@/i18n/format"
import { bytes } from "@/disk"
import { lastKey, planKey, type BackupStatus } from "@/backup"

/** Settings · 数据 · Hub 上的数据: where this Hub keeps its database and the copies of it, as the Hub says (an
 * installation's directory is its own), and when the last daily copy was made, a row each (a row's second line is cut after two lines). The Hub answers the instance owner
 * only (paths of its machine); anyone else sees nothing here. */
export function HubData() {
  const [s, setS] = useState<BackupStatus | null>(null)
  const [err, setErr] = useState<string | null>(null)
  const [hidden, setHidden] = useState(false), t = useT()
  useEffect(() => {
    modReq<BackupStatus>("GET", "/system/backup").then(setS)
      .catch((e) => { if (e instanceof HubError && e.status === 403) setHidden(true); else setErr(e instanceof Error ? e.message : String(e)) })
  }, [])
  if (hidden) return null
  const last = s && lastKey(s)
  return <Section data-hub-data label={t("manage:data.hub")}><Panel>
    <Row title={t("manage:data.location")} sub={<span className="type-code break-all" data-hub-database>{s?.database ?? err ?? "…"}</span>} />
    {s && <Row title={t("manage:data.backup")} sub={<span data-hub-backup={s.daily ? "daily" : "off"}>{t(`manage:data.${planKey(s)}`, { days: s.keep_days })}</span>} />}
    {s && <Row title={t("manage:data.backupDir")} sub={<span className="type-code break-all" data-hub-backup-dir>{s.dir}</span>} />}
    {s && last && <Row title={t("manage:data.backupLatest")} sub={<span data-hub-backup-last>{t(`manage:data.${last}`, s.last ? { when: monthDayTime(s.last.at), size: bytes(s.last.bytes) } : {})}</span>} />}
  </Panel></Section>
}
