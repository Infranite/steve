import { useEffect, useState } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { TriggerSection } from "../../src/channels/account"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { RecordRow } from "../../src/thread/conversations"
import { conversationsApi, type ConvRecord } from "../../src/api/conversations"
import { channelsApi, type Trigger, type Space } from "../../src/api/channels"
import { modReq } from "../../src/api"

function Settings({ id }: { id: string }) {
  const [trigger, setTrigger] = useState<Trigger | null>(null), [spaces, setSpaces] = useState<Space[]>([]), [busy, setBusy] = useState(false)
  const reload = async () => { const [account, list] = await Promise.all([modReq<{ trigger: Trigger }>("GET", `/channels/${id}`), channelsApi.spaces({ connection: id })]); setTrigger(account.trigger); setSpaces(list) }
  useEffect(() => { void reload() }, [id])
  if (!trigger) return <p>Loading fixture account</p>
  return <div className="mx-auto max-w-2xl p-4"><TriggerSection a={{ connection: { id }, trigger, spaces }} busy={busy} onSave={async (patch) => { setBusy(true); try { await channelsApi.patchAccount(id, { trigger: patch }); await reload(); return true } finally { setBusy(false) } }} /></div>
}

function Fixture() {
  const query = new URLSearchParams(location.search)
  const [records, setRecords] = useState<ConvRecord[]>([])
  const space = query.get("space")
  useEffect(() => { if (space) void conversationsApi.record(`chan:${space}`).then((r) => setRecords(r?.records ?? [])) }, [space])
  return <SidebarProvider><main className="w-full min-w-0">
    {space ? <div className="mx-auto flex max-w-2xl flex-col gap-4 p-4">{records.map((r) => <RecordRow key={r.id} r={r} />)}</div>
      : <Settings id={query.get("account") ?? "missing"} />}
  </main></SidebarProvider>
}
createRoot(document.getElementById("root")!).render(<Fixture />)
