/* 设备标签（B7）：给每台设备（包括 Hub 自己，即 local）打标签，agent 和流程可以按标签派发工作。 */
import { useEffect, useState } from "react"
import { TagIcon, XIcon } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { api } from "@/api"
import { Panel } from "@/panel"
import { useT } from "@/i18n"

export function NodeTags({ node }: { node: string }) {
  const t = useT()
  const [tags, setTags] = useState<string[] | null>(null)
  const [draft, setDraft] = useState(""), [error, setError] = useState("")
  useEffect(() => {
    let live = true
    void api.nodeTags().then(r => { if (live) setTags(r.tags[node] ?? []) }).catch(() => { if (live) setTags([]) })
    return () => { live = false }
  }, [node])
  if (tags === null) return null
  const save = async (next: string[]) => {
    setError("")
    try { setTags((await api.setNodeTags(node, next)).tags) } catch (e) { setError((e as Error).message) }
  }
  const add = () => {
    const v = draft.trim().toLowerCase()
    if (!v || tags.includes(v)) { setDraft(""); return }
    setDraft("")
    void save([...tags, v])
  }
  return (
    <Panel k="tags" data-node-tags icon={<TagIcon />} title={t("shell:devices.tags.title")} sub={t("shell:devices.tags.sub")}>
      <div className="px-5 py-3.5">
        <div className="flex flex-wrap items-center gap-1.5">
          {tags.map(tag => <Badge key={tag} variant="outline" className="h-6 gap-1 rounded-md font-mono text-[12px] font-normal">{tag}
            <button type="button" aria-label={t("shell:devices.tags.remove", { tag })} onClick={() => void save(tags.filter(x => x !== tag))}><XIcon className="size-3" /></button></Badge>)}
          <input className="min-w-28 flex-1 bg-transparent type-callout outline-none placeholder:text-tertiary" value={draft} placeholder={t("shell:devices.tags.add")}
            onChange={e => setDraft(e.target.value)} onKeyDown={e => { if (e.key === "Enter") { e.preventDefault(); add() } }} onBlur={add} data-node-tag-input />
        </div>
        {error && <div className="mt-1.5 type-footnote text-destructive" role="alert">{error}</div>}
      </div>
    </Panel>
  )
}
