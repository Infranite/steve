/* 经 tailcat 接入远程 Node（A6）：所有者开关；Hub 只把 Node 接口放进隧道。 */
import { useEffect, useState } from "react"
import { Button } from "@/components/ui/button"
import { api, type RInstall, type RTailcat } from "@/api"
import { Input } from "@/components/ui/input"
import { useT } from "@/i18n"

/** a command or code to copy: a raised line in the code face, on the muted block it sits in */
export const CODE = "block select-all break-all rounded-lg bg-background px-3 py-2 type-code text-foreground shadow-[var(--lift)]"

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
    <div className="space-y-2 rounded-xl bg-muted/60 p-4 type-callout" data-tailcat>
      <p className="type-body font-medium">{t("shell:devices.tailcat.title")}</p>
      <p className="text-muted-foreground">{s.available ? t("shell:devices.tailcat.help") : t("shell:devices.tailcat.missing")}</p>
      {error && <p role="alert" className="text-destructive">{error}</p>}
      {s.enabled && !s.running && s.error && <p className="text-destructive">{s.error}</p>}
      {s.enabled && s.hub_url && <>
        <p className="text-muted-foreground">{t("shell:devices.tailcat.command")}</p>
        <code className={CODE} data-tailcat-command>steve setup node --hub {s.hub_url}</code>
        <p className="text-muted-foreground">{t("shell:devices.tailcat.secret")}</p>
      </>}
      {(s.available || s.enabled) && <Button variant="outline" disabled={busy} onClick={() => void toggle()}>{s.enabled ? t("shell:devices.tailcat.off") : t("shell:devices.tailcat.on")}</Button>}
      {s.available && <RemoteInstall />}
    </div>
  )
}

/* 远程安装（A6b）：目标机器上运行一行 tailcat serve，把地址贴回来，其余由 Hub 完成。 */
function RemoteInstall() {
  const t = useT()
  const [op, setOp] = useState<RInstall | null>(null)
  const [address, setAddress] = useState("")
  const [busy, setBusy] = useState(false), [error, setError] = useState("")
  const act = async (f: () => Promise<void>) => {
    setBusy(true); setError("")
    try { await f() } catch (e) { setError((e as Error).message) } finally { setBusy(false) }
  }
  useEffect(() => {
    if (op?.state !== "running") return
    const timer = setInterval(() => { void api.install(op.id).then(setOp).catch(() => {}) }, 1000)
    return () => clearInterval(timer)
  }, [op?.id, op?.state])
  if (!op) return <div className="pt-2 space-y-2" data-remote-install>
    {error && <p role="alert" className="text-destructive">{error}</p>}
    <Button variant="outline" disabled={busy} onClick={() => void act(async () => { setOp(await api.createInstall()) })}>{t("shell:devices.install.start")}</Button>
  </div>
  return (
    <div className="anim-in space-y-2 border-t border-border/70 pt-3" data-remote-install>
      <p className="type-body font-medium">{t("shell:devices.install.title")}</p>
      {op.state === "waiting" || op.state === "failed" ? <>
        <p className="text-muted-foreground">{t("shell:devices.install.step1")}</p>
        <code className={CODE} data-install-command>{op.command}</code>
        <label className="block text-muted-foreground" htmlFor="install-address">{t("shell:devices.install.step2")}</label>
        <Input id="install-address" className="font-mono" value={address} onChange={e => setAddress(e.target.value)} placeholder="tc…" autoComplete="off" spellCheck={false} data-install-address />
        <Button disabled={busy || !address.trim()} onClick={() => void act(async () => { setOp(await api.runInstall(op.id, address)) })}>{op.state === "failed" ? t("shell:devices.install.retry") : t("shell:devices.install.run")}</Button>
      </> : null}
      {op.steps.length > 0 && <ol className="list-decimal pl-5 space-y-1" data-install-steps>{op.steps.map((s, i) => <li key={i} className={i === op.steps.length - 1 && op.state === "running" ? "font-medium" : "text-muted-foreground"}>{s.text}</li>)}</ol>}
      {op.state === "done" && <p data-install-done>{t("shell:devices.install.done", { id: op.node_id ?? "" })}</p>}
      {op.state === "failed" && op.error && <p role="alert" className="text-destructive whitespace-pre-wrap">{op.error}</p>}
      {error && <p role="alert" className="text-destructive">{error}</p>}
    </div>
  )
}
