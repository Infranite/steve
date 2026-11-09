/* 模型选择器：从 harness 实际能用的模型里挑（GET /api/system/models?harness=…），按家族分组、可搜索；
 * 列表外的模型才用「其他…」手填。Bot 编辑器、设置 · 模型、新会话默认都用这一个。 */
import { ago as fmtAgo } from "@/i18n/format"
import { useEffect, useState } from "react"
import { cn } from "cn"
import { ChevronsUpDownIcon, PencilIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Command, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from "@/components/ui/command"
import { Input } from "@/components/ui/input"
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover"
import { modReq } from "@/api"
import { runtimeName } from "@/store"
import { t, useT } from "@/i18n"

export type Used = { calls: number; ok_at: number | null; failed_at: number | null }
export type CatalogModel = { id: string; label: string; family: string; via: string | null; note: string | null; efforts: string[]; used?: Used }
export type Catalog = { harness: string; name: string; source: string; default: string | null; families: string[]; models: CatalogModel[] }

export const HARNESS_NAME: Record<string, string> = { "claude-code": "Claude Code", codex: "Codex", "claude-cli": "Claude Code", "codex-cli": "Codex" }
export const harnessName = (h: string) => runtimeName(h) ?? HARNESS_NAME[h] ?? h

const cache = new Map<string, Promise<Catalog>>()
/** the harness's model list, fetched once per page load (`fresh` asks the Hub to read it again) */
export function loadCatalog(harness: string, fresh = false): Promise<Catalog> {
  if (fresh || !cache.has(harness)) {
    const p = modReq<Catalog>("GET", `/system/models?harness=${encodeURIComponent(harness)}${fresh ? "&fresh=1" : ""}`)
    p.catch(() => cache.delete(harness))
    cache.set(harness, p)
  }
  return cache.get(harness)!
}
export function useCatalog(harness: string | null | undefined): Catalog | null {
  const [c, setC] = useState<Catalog | null>(null)
  useEffect(() => {
    let on = true
    setC(null)
    if (harness) loadCatalog(harness).then((x) => on && setC(x), () => on && setC({ harness, name: harnessName(harness), source: t("shell:model.unreadable"), default: null, families: [], models: [] }))
    return () => { on = false }
  }, [harness])
  return c
}

const ago = (t: number) => fmtAgo(t, { justNow: false })
/** a dot and a few words about what the statistics saw this week */
export function UsedMark({ u, className }: { u?: Used; className?: string }) {
  const t = useT()
  if (!u) return null
  const failedLast = u.failed_at != null && (u.ok_at == null || u.failed_at > u.ok_at)
  const tip = [t("shell:model.calls7d", { count: u.calls }), u.ok_at && t("shell:model.lastOk", { ago: ago(u.ok_at) }), u.failed_at && t("shell:model.lastFailed", { ago: ago(u.failed_at) })].filter(Boolean).join(" · ")
  return <span className={cn("inline-flex shrink-0 items-center gap-1 type-footnote text-muted-foreground", className)} title={tip} data-used={failedLast ? "failed" : "ok"}>
    <span className={cn("size-1.5 rounded-full", failedLast ? "bg-attention" : "bg-success")} />{failedLast ? t("shell:model.failedRecently") : t("shell:model.usedRecently", { count: u.calls })}
  </span>
}

/**
 * A searchable model select. `value` "" is the default (shown as `defaultLabel` when `allowDefault`).
 * Picking calls `onChange` with the id ("" for the default).
 */
export function ModelPicker({ harness, value, onChange, allowDefault, defaultLabel, testId, className, placeholder, exclude = [] }: {
  harness: string; value: string; onChange: (m: string) => void; allowDefault?: boolean; defaultLabel?: string; testId?: string; className?: string; placeholder?: string; exclude?: string[]
}) {
  const cat = useCatalog(harness), t = useT()
  const [open, setOpen] = useState(false)
  const [other, setOther] = useState(false)
  const [free, setFree] = useState("")
  const [q, setQ] = useState("")
  const cur = cat?.models.find((m) => m.id === value)
  const defLabel = defaultLabel ?? (cat?.default ? t("shell:model.defaultIs", { model: cat.default }) : t("shell:model.defaultOwn", { harness: harnessName(harness) }))
  const pick = (m: string) => { onChange(m); setOpen(false); setOther(false); setQ("") }
  const models = (cat?.models ?? []).filter((m) => !exclude.includes(m.id))
  const fams = (cat?.families ?? []).filter((f) => models.some((m) => m.family === f))
  return (
    <Popover open={open} onOpenChange={(o) => { setOpen(o); if (!o) { setOther(false); setQ("") } }}>
      <PopoverTrigger render={<Button variant="outline" size="sm" role="combobox" aria-expanded={open} className={cn("h-8 w-full min-w-0 justify-between gap-2 rounded-lg px-2.5 font-normal", className)} data-model-picker={testId ?? harness} />}>
        <span className={cn("min-w-0 truncate text-left", value ? "font-mono text-[12.5px]" : "text-muted-foreground")}>{value ? (cur?.label && cur.label !== value ? `${cur.label} · ${value}` : value) : allowDefault ? defLabel : placeholder ?? t("shell:model.choose")}</span>
        <ChevronsUpDownIcon className="size-3.5 shrink-0 opacity-50" />
      </PopoverTrigger>
      <PopoverContent align="start" className="w-[min(380px,calc(100vw-24px))] gap-0 rounded-2xl p-0" data-model-menu={testId ?? harness}>
        {other ? (
          <div className="flex flex-col gap-2 p-3">
            <div className="type-footnote text-muted-foreground">{t("shell:model.otherHint", { harness: harnessName(harness) })}</div>
            <div className="flex items-center gap-1.5">
              <Input autoFocus value={free} onChange={(e) => setFree(e.target.value)} placeholder={t("shell:model.id")} aria-label={t("shell:model.id")} className="h-8 font-mono text-[12.5px]" data-model-free
                onKeyDown={(e) => { if (e.key === "Enter" && !e.nativeEvent.isComposing && free.trim()) pick(free.trim()) }} />
              <Button size="sm" disabled={!free.trim()} onClick={() => pick(free.trim())}>{t("shell:model.useThis")}</Button>
            </div>
            <Button size="sm" variant="ghost" className="self-start text-muted-foreground" onClick={() => setOther(false)}>{t("shell:model.backToList")}</Button>
          </div>
        ) : (
          <Command className="rounded-2xl" defaultValue={cur ? `${cur.id} ${cur.label} ${cur.family}` : value ? undefined : "__default"} filter={(v, search) => (v.toLowerCase().includes(search.toLowerCase()) ? 1 : 0)}>
            <CommandInput placeholder={t("shell:model.search", { harness: harnessName(harness) })} value={q} onValueChange={setQ} data-model-search />
            <CommandList className="max-h-[min(360px,55vh)]">
              <CommandEmpty>{cat ? t("shell:model.noMatch") : t("shell:model.loading")}</CommandEmpty>
              {allowDefault && !q && <CommandGroup><CommandItem value="__default" data-checked={!value} onSelect={() => pick("")} data-model-opt="">{defLabel}</CommandItem></CommandGroup>}
              {fams.map((f) => (
                <CommandGroup key={f} heading={f}>
                  {models.filter((m) => m.family === f).map((m) => (
                    <CommandItem key={m.id} value={`${m.id} ${m.label} ${f}`} data-checked={m.id === value} onSelect={() => pick(m.id)} data-model-opt={m.id} className="items-start">
                      <span className="flex min-w-0 flex-1 flex-col">
                        <span className="flex min-w-0 items-center gap-1.5">
                          <span className="truncate font-mono text-[12.5px]">{m.via ? m.id.slice(m.via.length + 1) : m.id}</span>
                          {m.via && <span className="shrink-0 rounded bg-muted px-1 text-[10.5px] text-muted-foreground">{t("shell:model.via", { via: m.via })}</span>}
                        </span>
                        {(m.note || m.used) && <span className="flex min-w-0 items-center gap-2">{m.used && <UsedMark u={m.used} />}{m.note && <span className="truncate type-footnote text-muted-foreground" title={m.note}>{m.note}</span>}</span>}
                      </span>
                    </CommandItem>
                  ))}
                </CommandGroup>
              ))}
            </CommandList>
            <div className="flex items-center justify-between gap-2 border-t px-3 py-2">
              <span className="min-w-0 truncate type-footnote text-muted-foreground" title={cat?.source}>{cat ? t("shell:model.count", { count: models.length, source: cat.source }) : ""}</span>
              <Button size="xs" variant="ghost" className="shrink-0 text-muted-foreground" onClick={() => { setFree(value); setOther(true) }} data-model-other><PencilIcon />{t("shell:model.other")}</Button>
            </div>
          </Command>
        )}
      </PopoverContent>
    </Popover>
  )
}
