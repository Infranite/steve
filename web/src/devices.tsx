/* 设备页：Hub 是中心节点；运行环境是这台机器上实际检测到的 harness。配对其他设备需要 steve-node，还没有做。 */
import { useState } from "react"
import { Trans } from "react-i18next"
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
import { t, useT } from "@/i18n"

const Ico = ({ d, size = 36 }: { d: RDevice; size?: number }) => <span className="relative flex shrink-0 items-center justify-center rounded-xl bg-muted text-muted-foreground" style={{ width: size, height: size }}><ServerIcon className="size-[45%]" strokeWidth={1.5} />
  <span className={cn("absolute -bottom-0.5 -right-0.5 size-2.5 rounded-full ring-2 ring-background", d.status === "暂停接活" ? "bg-attention" : "bg-success")} /></span>
const onDevice = (id: string) => Object.keys(st.S).filter((k) => st.S[k].device === id && !st.S[k].ro)
const stateText = (d: RDevice) => (d.status === "暂停接活" ? t("shell:devices.onlinePaused") : t("shell:devices.online"))

export function DevicesView() {
  const t = useT()
  const ds = st.devices, hub = ds.find((d) => d.roles.includes("Hub")), rest = ds.filter((d) => d !== hub)
  return (
    <Page title={t("shell:devices.title")} sub={t("shell:devices.sub")} max={760}
      actions={<Button size="sm" variant="outline" onClick={() => openSheet("pair")} data-pair><PlusIcon />{t("shell:devices.add")}</Button>}>
      <div className="flex flex-col gap-8">
        {hub && <section><SectionLabel>Hub</SectionLabel>
          <Group><Row icon={<Ico d={hub} />} onClick={() => go("#/devices/" + hub.id)} data-device-row={hub.name} title={<span className="inline-flex items-center gap-1.5">{hub.name}{hub.roles.map((r) => <Badge key={r} variant="outline" className="font-normal text-muted-foreground">{r}</Badge>)}</span>}
            sub={<Meta><span>{stateText(hub)}</span><span>{t("shell:devices.running", { count: onDevice(hub.id).filter((k) => st.S[k].st === "run").length })}</span><span>{st.runtimes.filter((r) => r.ok).map((r) => r.name).join(t("shell:list.sep")) || t("shell:devices.noHarness")}</span></Meta>}><OwnerChip objectType="device" objectId={hub.id} bare /><ChevronRightIcon className="size-4 text-tertiary" /></Row></Group></section>}
        <section><SectionLabel>{t("shell:devices.others", { count: rest.length })}</SectionLabel>
          {rest.length ? <Group>{rest.map((d) => <Row key={d.id} icon={<Ico d={d} />} title={d.name} sub={d.roles.join(" · ")} onClick={() => go("#/devices/" + d.id)}><OwnerChip objectType="device" objectId={d.id} bare /></Row>)}</Group>
            : <Empty title={t("shell:devices.noOthers")} sub={t("shell:devices.noOthersSub")} />}</section>
        <section><SectionLabel>{t("shell:devices.scheduling")}</SectionLabel>
          <Group><Row title={t("shell:devices.where")} sub={t("shell:devices.whereSub")}><span className="type-footnote text-muted-foreground">{t("shell:devices.allOnHub")}</span></Row></Group></section>
      </div>
    </Page>
  )
}

export function DeviceView({ id }: { id: string }) {
  const [busy, setBusy] = useState(false), t = useT()
  const d = st.devices.find((x) => x.id === id)
  if (!d) return <Page title={t("shell:devices.title")} back={["#/devices", t("shell:devices.title")]}><Empty title={t("shell:devices.notFound")} /></Page>
  const isHub = d.roles.includes("Hub"), ss = onDevice(d.id).filter((k) => st.S[k].st !== "ended"), dirs = st.projects.filter((p) => p.device_id === d.id)
  const recheck = async () => { setBusy(true); try { await api.devices(true); await refresh(); toast(t("shell:devices.rechecked")) } catch (e) { toast((e as Error).message) } finally { setBusy(false) } }
  return (
    <Page title={d.name} back={["#/devices", t("shell:devices.title")]} max={720} sub={<Meta><span>{d.roles.join(" · ")}</span><span>{stateText(d)}</span><span>{d.os}</span><span>{d.version}</span></Meta>}
      actions={<Button size="sm" variant="outline" onClick={() => void api.patchDevice(d.id, { status: d.status === "暂停接活" ? "正常" : "暂停接活" }).then(() => { toast(d.status === "暂停接活" ? t("shell:devices.resumed") : t("shell:devices.paused")) ; return refresh() })} data-pause-device>{d.status === "暂停接活" ? <><PlayIcon />{t("shell:devices.resume")}</> : <><PauseIcon />{t("shell:devices.pause")}</>}</Button>}>
      <div className="flex flex-col gap-8">
        {isHub && <section><SectionLabel>{t("shell:devices.hubNode")}</SectionLabel><Group>
          <Row title={t("shell:devices.storage")} sub={t("shell:devices.storageSub")} />
          <Row title={t("shell:devices.access")} sub={t("shell:devices.accessSub")} />
          <Row title={t("shell:devices.models")} sub={t("shell:devices.modelsSub")} />
        </Group></section>}
        <section><SectionLabel right={<span className="type-footnote font-normal">{t("shell:devices.maxSessions", { count: d.max_sessions })}</span>}>{t("shell:devices.sessionsHere", { count: ss.length })}</SectionLabel>
          {ss.length ? <Group>{ss.map((k) => { const s = st.S[k]; return <Row key={k} title={s.title} sub={s.item ? t("shell:devices.sessionSub", { item: s.item, n: s.n, up: s.up }) : t("shell:devices.noItem")} onClick={() => go("#/sessions/" + k)}><Status s={s.st} sess /></Row> })}</Group> : <Empty title={t("shell:devices.noSessions")} />}</section>
        <section><SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" disabled={busy} onClick={() => void recheck()}>{busy ? <Spinner className="size-3" /> : <RefreshCwIcon />}{t("shell:devices.recheck")}</Button>}>{t("shell:devices.runtimes")}</SectionLabel>
          {st.runtimes.length ? <Group>{st.runtimes.map((r) => <Row key={r.id} icon={<span className="flex size-8 items-center justify-center rounded-lg bg-muted text-muted-foreground"><CpuIcon className="size-4" strokeWidth={1.5} /></span>}
            title={<span className="inline-flex items-center gap-1.5">{r.name}<span className="type-footnote text-muted-foreground">{r.version}</span></span>} sub={<span>{r.note} · <span className="font-mono">{r.path.replace(/^\/Users\/[^/]+/, "~")}</span></span>}>
            <span className="inline-flex items-center gap-1.5 type-footnote text-muted-foreground"><StatusDot s={r.ok ? "done" : "wait"} />{r.ok ? t("shell:devices.available") : t("shell:devices.unavailable")}</span></Row>)}</Group> : <Empty title={t("shell:devices.noRuntimes")} />}</section>
        <section><SectionLabel right={<Button size="xs" variant="ghost" className="text-muted-foreground" onClick={() => go("#/projects")}>{t("shell:devices.manageProjects")}</Button>}>{t("shell:devices.projectsHere", { count: dirs.length })}</SectionLabel>
          {dirs.length ? <Group>{dirs.map((p) => <Row key={p.id} icon={<FolderIcon className="size-4 text-muted-foreground" strokeWidth={1.5} />} title={p.name} sub={<span className="font-mono">{p.path.replace(/^\/Users\/[^/]+/, "~")}</span>} onClick={() => go("#/projects/" + p.id)}><ChevronRightIcon className="size-4 text-tertiary" /></Row>)}</Group>
            : <Empty title={t("shell:devices.noProjects")} />}</section>
        <section><SectionLabel>{t("shell:devices.connection")}</SectionLabel><Group><Row title={t("shell:devices.lastSeen")} sub={tm(d.seen)} /><Row title={t("shell:devices.owner")} sub={t("shell:devices.ownerSub")}><OwnerChip objectType="device" objectId={d.id} bare /></Row></Group></section>
      </div>
    </Page>
  )
}

export function PairDialog() {
  const t = useT()
  return (
    <Dialog open onOpenChange={(x) => !x && closeSheet()}>
      <DialogContent className="gap-4 rounded-2xl sm:max-w-[480px]" data-pair-dialog>
        <DialogHeader><DialogTitle className="type-headline">{t("shell:devices.add")}</DialogTitle><DialogDescription className="type-callout">{t("shell:devices.pairDesc")}</DialogDescription></DialogHeader>
        <div className="rounded-xl border px-4 py-3 type-callout text-muted-foreground"><Trans t={t} i18nKey="shell:devices.pairNotYet" components={{ code: <span className="font-mono text-foreground" /> }} /></div>
        <DialogFooter><Button onClick={closeSheet}>{t("shell:devices.gotIt")}</Button></DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
