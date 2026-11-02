/* 设置 · 保险箱 (accounts-vault.md §8): passkeys for local verification, the key store and its state, recovery and key rotation. */
import { useState } from "react"
import { LockIcon, TrashIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Spinner } from "@/components/ui/spinner"
import { AlertDialog, AlertDialogAction, AlertDialogCancel, AlertDialogContent, AlertDialogDescription, AlertDialogFooter, AlertDialogHeader, AlertDialogTitle } from "@/components/ui/alert-dialog"
import { Group, Row, SectionLabel, Empty, go } from "@/ui"
import { tm } from "@/store"
import { vaultApi, keyStoreState, keyStoreWhere } from "@/api/vault"
import { useLoad, useBusy } from "@/accounts/load"
import { RegisterPasskey } from "@/vault/reveal"
import { RecoveryCode } from "@/vault/index"

/** the settings category: [id, label, icon, description] like manage.tsx CATS */
export const VAULT_CAT: [string, string, any, string] = ["vault", "保险箱", LockIcon, "通行密钥、恢复码、主密钥"]

export function VaultSettingsPane() {
  const L = useLoad(async () => {
    const status = await vaultApi.status()
    return { status, passkeys: status.initialized ? await vaultApi.passkeys().catch(() => null) : null }
  }, [], ["vault"])
  const [busy, act] = useBusy(), [code, setCode] = useState(""), [rotated, setRotated] = useState<string | null>(null), [ask, setAsk] = useState(false), [rm, setRm] = useState<{ id: string; name: string } | null>(null)
  if (L.s === "loading") return <div className="flex justify-center py-12"><Spinner className="size-5 text-muted-foreground" /></div>
  if (L.s === "unwired") return <Empty title="还没有接入" sub="这个 Hub 还没有保险箱。" />
  if (L.s === "error") return <Empty title={L.err} />
  const { status: s, passkeys } = L.data
  if (!s.initialized) return <Group><Row title="保险箱还没有启用" sub="启用后可以登记通行密钥、管理恢复码" onClick={() => go("#/vault")}><Button size="sm" variant="outline" tabIndex={-1}>去启用</Button></Row></Group>
  if (rotated) return <RecoveryCode title="新的恢复码" code={rotated} onDone={() => { setRotated(null); void L.reload() }} />
  return (
    <div className="flex flex-col gap-8" data-vault-settings-pane>
      <AlertDialog open={!!rm} onOpenChange={(o) => !o && setRm(null)}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>删除通行密钥“{rm?.name}”？</AlertDialogTitle><AlertDialogDescription>删除后这台设备上不能再用它查看原值，要重新登记。</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel><AlertDialogAction variant="destructive" onClick={async () => { if (rm && await act(() => vaultApi.deletePasskey(rm.id), "已删除")) void L.reload() }}>删除</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
      <AlertDialog open={ask} onOpenChange={setAsk}><AlertDialogContent>
        <AlertDialogHeader><AlertDialogTitle>轮换主密钥？</AlertDialogTitle><AlertDialogDescription>Steve 生成新的主密钥，在后台把每个条目的数据密钥重新包裹一遍。完成后旧的主密钥和旧的恢复码都作废，你要抄下新的恢复码。</AlertDialogDescription></AlertDialogHeader>
        <AlertDialogFooter><AlertDialogCancel>取消</AlertDialogCancel><AlertDialogAction onClick={async () => { const r = await act(() => vaultApi.rotate()); if (r) setRotated(r.recovery_code) }} data-rotate-confirm>轮换</AlertDialogAction></AlertDialogFooter>
      </AlertDialogContent></AlertDialog>
      <section><SectionLabel>本机验证</SectionLabel>
        {passkeys?.length ? <Group>{passkeys.map((p) => <Row key={p.id} title={p.name} sub={`${p.rp_id} · 登记于 ${tm(p.created)}${p.last_used ? " · 上次使用 " + tm(p.last_used) : ""}`} data-passkey={p.name}>
          <Button variant="ghost" size="icon-xs" aria-label={"删除通行密钥 " + p.name} className="text-muted-foreground" onClick={() => setRm(p)}><TrashIcon /></Button></Row>)}</Group>
          : <div className="px-1 type-callout text-muted-foreground">还没有登记通行密钥。查看或复制机密字段之前要先登记。</div>}
        <div className="mt-3"><RegisterPasskey onDone={() => void L.reload()} /></div>
        <div className="mt-2 px-1 type-footnote text-muted-foreground">Mac 和 iPhone 上用 Touch ID / Face ID。每个访问 Steve 的地址（域名）要各登记一次；登记要在登录后 10 分钟内完成。</div>
      </section>
      <section><SectionLabel>主密钥</SectionLabel><Group>
        <Row title="存放位置" sub={s.keystore?.label ?? "macOS 钥匙串"} data-keystore={s.keystore?.kind ?? "keychain"} />
        <Row title="状态" sub={s.keychain === "ok" ? `主密钥存在：${s.keystore?.label ?? "macOS 钥匙串"}` : keyStoreState(s)} data-keychain-state={s.keychain} />
        <Row title="版本" sub={s.rotating ? "正在轮换，旧版本暂时保留" : `主密钥只在${keyStoreWhere(s)}里，数据库和备份里只有密文`}><span className="type-callout tabular-nums text-muted-foreground">v{s.kek_version}</span></Row>
        <Row title="轮换主密钥" sub="怀疑恢复码泄露时用；会生成新的恢复码"><Button size="sm" variant="outline" disabled={busy || s.rotating || s.keychain !== "ok"} onClick={() => setAsk(true)} data-rotate>轮换</Button></Row>
      </Group></section>
      <section><SectionLabel>用恢复码恢复</SectionLabel>
        <form className="flex flex-wrap items-center gap-2" autoComplete="off" onSubmit={async (e) => { e.preventDefault(); const r = await act(() => vaultApi.recover(code.trim()), "已恢复主密钥"); setCode(""); if (r) void L.reload() }}>
          <Input aria-label="恢复码" type="password" autoComplete="off" spellCheck={false} className="h-8 min-w-0 flex-1 rounded-lg font-mono" value={code} onChange={(e) => setCode(e.target.value)} placeholder="换机器或钥匙串丢了时填写" data-recover-code />
          <Button size="sm" variant="outline" type="submit" disabled={busy || !code.trim()} data-recover>恢复</Button>
        </form>
        <div className="mt-2 px-1 type-footnote text-muted-foreground">恢复码和保险箱的主密钥对得上才会写回钥匙串。</div>
      </section>
    </div>
  )
}
