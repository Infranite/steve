/* 经 tailcat 接入远程 Node（A6）：所有者开关；Hub 只把 Node 接口放进隧道。 */
import { useEffect, useState } from "react"
import { Button } from "@/components/ui/button"
import { api, type RTailcat } from "@/api"
import { useT } from "@/i18n"

export function TailcatSection() {
  const t = useT()
  const [s, setS] = useState<RTailcat | null>(null)
  const [busy, setBusy] = useState(false), [error, setError] = useState("")
  useEffect(() => {
    let live = true
    void api.nodesTailcat().then(v => { if (live) setS(v) }).catch(() => {})
    return () => { live = false }
  }, [])
  if (!s) return null
  const toggle = async () => {
    setBusy(true); setError("")
    try { setS(await api.setNodesTailcat(!s.enabled)) }
    catch (e) { setError((e as Error).message); void api.nodesTailcat().then(setS).catch(() => {}) }
    finally { setBusy(false) }
  }
  return (
    <div className="rounded-xl border p-4 space-y-2 type-callout" data-tailcat>
      <p className="font-medium">{t("shell:devices.tailcat.title")}</p>
      <p className="text-muted-foreground">{s.available ? t("shell:devices.tailcat.help") : t("shell:devices.tailcat.missing")}</p>
      {error && <p role="alert" className="text-destructive">{error}</p>}
      {s.enabled && !s.running && s.error && <p className="text-destructive">{s.error}</p>}
      {s.enabled && s.hub_url && <>
        <p>{t("shell:devices.tailcat.command")}</p>
        <code className="block break-all select-all" data-tailcat-command>steve setup node --hub {s.hub_url}</code>
        <p className="text-muted-foreground">{t("shell:devices.tailcat.secret")}</p>
      </>}
      {(s.available || s.enabled) && <Button variant="outline" disabled={busy} onClick={() => void toggle()}>{s.enabled ? t("shell:devices.tailcat.off") : t("shell:devices.tailcat.on")}</Button>}
    </div>
  )
}
