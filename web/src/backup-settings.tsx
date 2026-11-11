import { useEffect, useState } from "react"
import { HubError, modReq } from "@/api"
import { Group, Row, SectionLabel } from "@/ui"
import { useT } from "@/i18n"
import { monthDayTime } from "@/i18n/format"
import { bytes } from "@/disk"
import { lastKey, planKey, type BackupStatus } from "@/backup"

/** Settings · 数据 · Hub 上的数据: where this Hub keeps its database and the copies of it, as the Hub says (an
 * installation's directory is its own), and when the last daily copy was made. The Hub answers the instance owner
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
  return <section data-hub-data><SectionLabel>{t("manage:data.hub")}</SectionLabel><Group>
    <Row title={t("manage:data.location")} sub={<span className="font-mono break-all" data-hub-database>{s?.database ?? err ?? "…"}</span>} />
    {s && <Row title={t("manage:data.backup")} sub={<span data-hub-backup={s.daily ? "daily" : "off"}>
      {t(`manage:data.${planKey(s)}`, { dir: s.dir, days: s.keep_days })}
      {last && <><br /><span data-hub-backup-last>{t(`manage:data.${last}`, s.last ? { when: monthDayTime(s.last.at), size: bytes(s.last.bytes) } : {})}</span></>}
    </span>} />}
  </Group></section>
}
