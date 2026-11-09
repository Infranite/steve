/* 设备标签（B7）：给每台设备（包括 Hub 自己，即 local）打标签，agent 和流程可以按标签派发工作。 */
import { useEffect, useState } from "react"
import { XIcon } from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { api } from "@/api"
import { Group, Row, SectionLabel } from "@/ui"
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
    <section data-node-tags>
      <SectionLabel>{t("shell:devices.tags.title")}</SectionLabel>
      <Group>
        <Row title={<span className="flex flex-wrap items-center gap-1.5">
          {tags.map(tag => <Badge key={tag} variant="outline" className="gap-1 font-normal">{tag}
            <button type="button" aria-label={t("shell:devices.tags.remove", { tag })} onClick={() => void save(tags.filter(x => x !== tag))}><XIcon className="size-3" /></button></Badge>)}
          <input className="min-w-28 flex-1 bg-transparent type-callout outline-none" value={draft} placeholder={t("shell:devices.tags.add")}
            onChange={e => setDraft(e.target.value)} onKeyDown={e => { if (e.key === "Enter") { e.preventDefault(); add() } }} onBlur={add} data-node-tag-input />
        </span>} sub={error || t("shell:devices.tags.sub")} />
      </Group>
    </section>
  )
}
