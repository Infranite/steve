/* 待处理 cards of the vault and connections (accounts-vault.md §4.3, §2.2, §8):
 * - `secret_use` (approve): “<Bot> 想用「<条目>」：<用途>”, the command / directory / injection shown verbatim; once, a
 *   1 / 8 / 24 h grant, or deny. High items and bank cards only offer once / deny (the Hub sends the options).
 * - `connection` (fail): “<账号> 的授权过期了”; re-authorize (goes to the account page, which opens the browser flow) or pause.
 * Rendered from ui.tsx's DecisionCard with its header, footer and `act`. */
import { useState, type ReactNode } from "react"
import { ChevronDownIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { CardContent } from "@/components/ui/card"
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { api } from "@/api"
import { refresh, toast, type Wait } from "@/store"
import { report } from "@/accounts/load"
import { NewSpaceCard } from "@/channels/new-space"

type P = { w: Wait; act: (c: string, t?: string) => Promise<void>; busy: boolean; head: (t: ReactNode) => ReactNode; foot: (c: ReactNode) => ReactNode }
const GRANTS: [string, string][] = [["grant_1h", "授权 1 小时"], ["grant_8h", "授权 8 小时"], ["grant_24h", "授权 24 小时"]]

function SecretUseCard({ w, act, busy, head, foot }: P) {
  const grants = GRANTS.filter(([o]) => w.options.includes(o))
  return <>
    {head(w.q)}
    <CardContent className="pb-4" data-secret-use>
      {w.draft && <pre className="max-h-56 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-muted/70 px-3 py-2 font-mono text-[12px] leading-[18px] text-foreground/85">{w.draft}</pre>}
      <div className="mt-2 type-footnote text-muted-foreground">Steve 替它运行这条命令并注入机密，它只拿到退出码和打码后的输出。{grants.length ? "授权期间，同一个 Bot 在同一个范围里再用这个条目不再问你。" : "这个条目每次使用都要你批准。"}</div>
    </CardContent>
    {foot(<>
      {w.options.includes("deny") && <Button variant="ghost" className="mr-auto text-muted-foreground" disabled={busy} onClick={() => void act("deny")} data-secret-deny>拒绝</Button>}
      {grants.length > 0 && <DropdownMenu>
        <DropdownMenuTrigger render={<Button variant="outline" disabled={busy} data-secret-grant-menu />}>授权…<ChevronDownIcon /></DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="min-w-40">{grants.map(([o, l]) => <DropdownMenuItem key={o} onClick={() => void act(o)} data-secret-grant={o}>{l}</DropdownMenuItem>)}</DropdownMenuContent>
      </DropdownMenu>}
      {w.options.includes("once") && <Button disabled={busy} onClick={() => void act("once")} data-secret-once>批准这一次</Button>}
    </>)}
  </>
}

function ConnectionCard({ w, act, busy, head, foot }: P) {
  const [going, setGoing] = useState(false)
  /* the reauth answer carries the account; its page continues with the browser flow */
  const reauth = async () => {
    setGoing(true)
    try { const d = await api.decide(w.decision, "reauth"); await refresh(); const acc = (d as any)?.result?.account_id; if (acc) location.hash = `#/accounts/${acc}/reauth`; else toast("这个连接已经断开了") }
    catch (e) { report(e) } finally { setGoing(false) }
  }
  return <>
    {head(w.q)}
    <CardContent className="pb-4" data-connection-fail>
      {w.draft ? <div className="break-words type-body text-muted-foreground">{w.draft}</div>
        : <div className="type-callout text-muted-foreground">重新授权之前，这个账号收不到新消息和事件。暂停后不再提醒，恢复后也不补收。</div>}
    </CardContent>
    {foot(<>
      {w.options.includes("pause") && <Button variant="outline" disabled={busy || going} onClick={() => void act("pause")} data-conn-fail-pause>暂停接收</Button>}
      {w.options.includes("reauth") && <Button disabled={busy || going} onClick={() => void reauth()} data-conn-fail-reauth>重新授权</Button>}
    </>)}
  </>
}

export const MODULE_CARDS: Record<string, (p: P) => ReactNode> = {
  secret_use: (p) => <SecretUseCard {...p} />,
  connection: (p) => <ConnectionCard {...p} />,
  space: (p) => <NewSpaceCard {...p} />,
}
