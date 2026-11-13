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
import { t as tr, useT } from "@/i18n"

type P = { w: Wait; act: (c: string, t?: string) => Promise<void>; busy: boolean; head: (t: ReactNode) => ReactNode; foot: (c: ReactNode) => ReactNode }
/** the grant answers the Hub may offer, with their hours */
const GRANTS: [string, number][] = [["grant_1h", 1], ["grant_8h", 8], ["grant_24h", 24]]

function SecretUseCard({ w, act, busy, head, foot }: P) {
  const t = useT()
  const grants = GRANTS.filter(([o]) => w.options.includes(o))
  return <>
    {head(w.q)}
    <CardContent className="pb-4" data-secret-use>
      {w.draft && <pre className="max-h-56 overflow-auto whitespace-pre-wrap break-all rounded-lg bg-muted/60 px-3 py-2 type-code text-foreground/85">{w.draft}</pre>}
      <div className="mt-2 type-footnote text-muted-foreground">{grants.length ? t("vault:decision.secretUseNoteGrant") : t("vault:decision.secretUseNoteOnce")}</div>
    </CardContent>
    {foot(<>
      {w.options.includes("deny") && <Button variant="ghost" className="mr-auto text-muted-foreground" disabled={busy} onClick={() => void act("deny")} data-secret-deny>{t("vault:decision.deny")}</Button>}
      {grants.length > 0 && <DropdownMenu>
        <DropdownMenuTrigger render={<Button variant="outline" disabled={busy} data-secret-grant-menu />}>{t("vault:decision.grantMenu")}<ChevronDownIcon /></DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="min-w-40">{grants.map(([o, h]) => <DropdownMenuItem key={o} onClick={() => void act(o)} data-secret-grant={o}>{t("vault:decision.grantFor", { count: h })}</DropdownMenuItem>)}</DropdownMenuContent>
      </DropdownMenu>}
      {w.options.includes("once") && <Button disabled={busy} onClick={() => void act("once")} data-secret-once>{t("vault:decision.once")}</Button>}
    </>)}
  </>
}

function ConnectionCard({ w, act, busy, head, foot }: P) {
  const t = useT()
  const [going, setGoing] = useState(false)
  /* the reauth answer carries the account; its page continues with the browser flow */
  const reauth = async () => {
    setGoing(true)
    try { const d = await api.decide(w.decision, "reauth"); await refresh(); const acc = (d as any)?.result?.account_id; if (acc) location.hash = `#/accounts/${acc}/reauth`; else toast(tr("vault:decision.disconnected")) }
    catch (e) { report(e) } finally { setGoing(false) }
  }
  return <>
    {head(w.q)}
    <CardContent className="pb-4" data-connection-fail>
      {w.draft ? <div className="break-words type-body text-muted-foreground">{w.draft}</div>
        : <div className="type-callout text-muted-foreground">{t("vault:decision.connBody")}</div>}
    </CardContent>
    {foot(<>
      {w.options.includes("pause") && <Button variant="outline" disabled={busy || going} onClick={() => void act("pause")} data-conn-fail-pause>{t("vault:decision.pause")}</Button>}
      {w.options.includes("reauth") && <Button disabled={busy || going} onClick={() => void reauth()} data-conn-fail-reauth>{t("vault:decision.reauth")}</Button>}
    </>)}
  </>
}

export const MODULE_CARDS: Record<string, (p: P) => ReactNode> = {
  secret_use: (p) => <SecretUseCard {...p} />,
  connection: (p) => <ConnectionCard {...p} />,
  space: (p) => <NewSpaceCard {...p} />,
}
