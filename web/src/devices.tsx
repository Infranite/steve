/* 设备页：Hub 是中心节点；运行环境是这台机器上实际检测到的 harness。配对其他设备需要 steve-node，还没有做。 */
import { useState } from "react"
import { cn } from "cn"
import { ChevronRightIcon, PlusIcon, ServerIcon, CpuIcon, FolderIcon, PauseIcon, PlayIcon, RefreshCwIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Spinner } from "@/components/ui/spinner"
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog"
import { st, toast, refresh, tm } from "@/store"
import { api, type RDevice } from "@/api"
import { go, Page, Group, Row, SectionLabel, StatusDot, Meta, Empty, Status, closeSheet, openSheet } from "@/ui"
import { OwnerChip } from "@/people/owner-chip"

const Ico = ({ d, size = 36 }: { d: RDevice; size?: number }) => <span className="relative flex shrink-0 items-center justify-center rounded-xl bg-muted text-muted-foreground" style={{ width: size, height: size }}><ServerIcon className="size-[45%]" strokeWidth={1.5} />
  <span className={cn("absolute -bottom-0.5 -right-0.5 size-2.5 rounded-full ring-2 ring-background", d.status === "暂停接活" ? "bg-attention" : "bg-success")} /></span>
const onDevice = (id: string) => Object.keys(st.S).filter((k) => st.S[k].device === id && !st.S[k].ro)
const stateText = (d: RDevice) => (d.status === "暂停接活" ? "在线 · 暂停接活" : "在线")

export function DevicesView() {
  const ds = st.devices, hub = ds.find((d) => d.roles.includes("Hub")), rest = ds.filter((d) => d !== hub)
  return (
    <Page title="设备" sub="Hub 是中心节点：数据、调度和模型调用都在它上面。其他设备配对后可以替你跑会话，或者用来查看和审批" max={760}
      actions={<Button size="sm" variant="outline" onClick={() => openSheet("pair")} data-pair><PlusIcon />添加设备</Button>}>
      <div className="flex flex-col gap-8">
        {hub && <section><SectionLabel>Hub</SectionLabel>
          <Group><Row icon={<Ico d={hub} />} onClick={() => go("#/devices/" + hub.id)} data-device-row={hub.name} title={<span className="inline-flex items-center gap-1.5">{hub.name}{hub.roles.map((r) => <Badge key={r} variant="outline" className="font-normal text-muted-foreground">{r}</Badge>)}</span>}
            sub={<Meta><span>{stateText(hub)}</span><span>{onDevice(hub.id).filter((k) => st.S[k].st === "run").length} 个会话运行中</span><span>{st.runtimes.filter((r) => r.ok).map((r) => r.name).join("、") || "没有可用的 harness"}</span></Meta>}><OwnerChip objectType="device" objectId={hub.id} bare /><ChevronRightIcon className="size-4 text-tertiary" /></Row></Group></section>}
        <section><SectionLabel>其他设备 · {rest.length}</SectionLabel>
          {rest.length ? <Group>{rest.map((d) => <Row key={d.id} icon={<Ico d={d} />} title={d.name} sub={d.roles.join(" · ")} onClick={() => go("#/devices/" + d.id)}><OwnerChip objectType="device" objectId={d.id} bare /></Row>)}</Group>
            : <Empty title="还没有其他设备" sub="MacBook、GPU 服务器、手机都可以配对进来。配对要用到设备端的 steve-node，还没有做。" />}</section>
        <section><SectionLabel>调度</SectionLabel>
          <Group><Row title="会话在哪台设备上跑" sub="现在只有 Hub 一台执行节点，所有会话都在 Hub 上跑。配对更多设备后，按项目目录在哪、需要哪个 harness 来选"><span className="type-footnote text-muted-foreground">全部在 Hub</span></Row></Group></section>
      </div>
    </Page>
  )
}

export function DeviceView({ id }: { id: string }) {
  const [busy, setBusy] = useState(false)
  const d = st.devices.find((x) => x.id === id)
  if (!d) return <Page title="设备" back={["#/devices", "设备"]}><Empty title="设备不存在" /></Page>
  const isHub = d.roles.includes("Hub"), ss = onDevice(d.id).filter((k) => st.S[k].st !== "ended"), dirs = st.projects.filter((p) => p.device_id === d.id)
  const recheck = async () => { setBusy(true); try { await api.devices(true); await refresh(); toast("已重新检测 harness") } catch (e) { toast((e as Error).message) } finally { setBusy(false) } }
  return (
    <Page title={d.name} back={["#/devices", "设备"]} max={720} sub={<Meta><span>{d.roles.join(" · ")}</span><span>{stateText(d)}</span><span>{d.os}</span><span>{d.version}</span></Meta>}
      actions={<Button size="sm" variant="outline" onClick={() => void api.patchDevice(d.id, { status: d.status === "暂停接活" ? "正常" : "暂停接活" }).then(() => { toast(d.status === "暂停接活" ? "已恢复接活" : "已暂停接活：正在运行的会话继续；新事项会等你恢复") ; return refresh() })} data-pause-device>{d.status === "暂停接活" ? <><PlayIcon />恢复接活</> : <><PauseIcon />暂停接活</>}</Button>}>
      <div className="flex flex-col gap-8">
        {isHub && <section><SectionLabel>中心节点</SectionLabel><Group>
          <Row title="存储" sub="SQLite · ~/.steve/data；每天备份，发布前也备份" />
          <Row title="访问入口" sub="访问地址和登录方式由部署者配置；本机也可直接访问" />
          <Row title="模型调用" sub="Steve 自己的路由、回答和起草走本机的 Claude Code，不需要单独的 API Key" />
        </Group></section>}
        <section><SectionLabel right={<span className="type-footnote font-normal">同时最多 {d.max_sessions} 个会话</span>}>这台设备上的会话 · {ss.length}</SectionLabel>
          {ss.length ? <Group>{ss.map((k) => { const s = st.S[k]; return <Row key={k} title={s.title} sub={s.item ? `${s.item} · 会话 #${s.n} · ${s.up}` : "未关联事项"} onClick={() => go("#/sessions/" + k)}><Status s={s.st} sess /></Row> })}</Group> : <Empty title="没有未结束的会话" />}</section>
        <section><SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void recheck()}>{busy ? <Spinner className="size-3" /> : <RefreshCwIcon />}重新检测</Button>}>运行环境</SectionLabel>
          {st.runtimes.length ? <Group>{st.runtimes.map((r) => <Row key={r.id} icon={<span className="flex size-8 items-center justify-center rounded-lg bg-muted text-muted-foreground"><CpuIcon className="size-4" strokeWidth={1.5} /></span>}
            title={<span className="inline-flex items-center gap-1.5">{r.name}<span className="type-footnote text-muted-foreground">{r.version}</span></span>} sub={<span>{r.note} · <span className="font-mono">{r.path.replace(/^\/Users\/[^/]+/, "~")}</span></span>}>
            <span className="inline-flex items-center gap-1.5 type-footnote text-muted-foreground"><StatusDot s={r.ok ? "done" : "wait"} />{r.ok ? "可用" : "不可用"}</span></Row>)}</Group> : <Empty title="没有检测到 harness" />}</section>
        <section><SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => go("#/projects")}>管理项目</Button>}>这台设备上的项目目录 · {dirs.length}</SectionLabel>
          {dirs.length ? <Group>{dirs.map((p) => <Row key={p.id} icon={<FolderIcon className="size-4 text-muted-foreground" strokeWidth={1.5} />} title={p.name} sub={<span className="font-mono">{p.path.replace(/^\/Users\/[^/]+/, "~")}</span>} onClick={() => go("#/projects/" + p.id)}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Group>
            : <Empty title="当前工作空间在这台设备上没有项目" />}</section>
        <section><SectionLabel>连接</SectionLabel><Group><Row title="最近在线" sub={tm(d.seen)} /><Row title="负责人" sub="这台设备归谁管：出问题、要升级时找这个人"><OwnerChip objectType="device" objectId={d.id} bare /></Row></Group></section>
      </div>
    </Page>
  )
}

export function PairDialog() {
  return (
    <Dialog open onOpenChange={(x) => !x && closeSheet()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[480px]" data-pair-dialog>
        <DialogHeader><DialogTitle className="type-headline">添加设备</DialogTitle><DialogDescription className="type-callout">新设备主动连到 Hub，配对后拿到自己的设备证书，可以随时吊销。</DialogDescription></DialogHeader>
        <div className="rounded-xl border px-4 py-3 type-callout text-muted-foreground">设备端的 <span className="font-mono text-foreground">steve-node</span> 还没有做，现在不能配对。所有会话都在 Hub 上跑；手机和其他电脑可以直接用浏览器打开 Steve。</div>
        <DialogFooter><Button onClick={closeSheet}>知道了</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
