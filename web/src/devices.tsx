/* Devices: one page for the Hub in use and everything on it, in tabs: Overview (#/devices), the Hub
 * (#/devices/hub), its Nodes (#/devices/nodes, a Node at #/devices/nodes/<id>), the clients signed in to it
 * (#/devices/clients, a client's row at #/devices/clients/<id>), and Monitor (#/devices/monitor, owner only). */
import { useEffect } from "react"
import { NetworkIcon, PlusIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { SegmentedControl, type SegmentOption } from "@/components/affect/segmented-control-clean"
import { go, openSheet, Page } from "@/ui"
import { currentHub } from "@/hub"
import { useHubs } from "@/hub-ui"
import { useT } from "@/i18n"
import { nodeStatus } from "@/device-model"
import { mapClients } from "@/device-map-model"
import { overallStatus, sortedWarnings } from "@/health"
import { st, useStore } from "@/store"
import { DevicesProvider, useDevices } from "@/devices-data"
import { OverviewTab } from "@/devices-overview"
import { HubTab } from "@/devices-hub"
import { NodesTab } from "@/devices-nodes"
import { ClientsTab } from "@/devices-clients"
import { MonitorTab } from "@/devices-monitor"
import { NodePage } from "@/devices-node"
import { parseNodeTab } from "@/node-detail-model"
import "./devices.css"

const TABS = ["overview", "hub", "nodes", "clients", "monitor"] as const
type Tab = (typeof TABS)[number]
export const devicesHref = (tab: Tab) => (tab === "overview" ? "#/devices" : "#/devices/" + tab)

/** the route under #/devices: a tab or a Node's page (with optional tab segment) */
export function DevicesRoute({ b, c, d }: { b?: string; c?: string; d?: string }) {
  const tab: Tab = b && (TABS as readonly string[]).includes(b) ? (b as Tab) : "overview"
  return <DevicesProvider>{tab === "nodes" && c ? <NodePage key={c} id={decodeURIComponent(c)} tab={parseNodeTab(d)} /> : <DevicesPage tab={tab} focus={tab === "clients" && c ? decodeURIComponent(c) : undefined} />}</DevicesProvider>
}

function DevicesPage({ tab, focus }: { tab: Tab; focus?: string }) {
  useStore()
  const t = useT(), { views, hubVersion, access, health } = useDevices(), { hubs } = useHubs()
  const owner = st.owner
  useEffect(() => {
    if (tab === "monitor" && !owner) {
      history.replaceState(null, "", location.pathname + location.search + "#/devices")
    }
  }, [tab, owner])
  const cur = hubs?.find((h) => h.current)
  const live = views.filter((v) => v.state !== "revoked")
  const warn = live.filter((v) => {
    const s = nodeStatus(v, hubVersion)
    return s.kind === "offline" || s.tone === "warn" || s.tone === "bad"
  }).length
  const healthWarns = owner && health ? sortedWarnings(health) : []
  const healthCritical = owner && health ? overallStatus(health) === "critical" : false
  /* the clients as the overview and the map count them: one per client, this browser included even when its sign-in
   * is no registered client; the head says how many are online now, the tab how many are signed in */
  const counted = access ? mapClients(access, t("devices:clients.browserSession")) : null
  const clients = counted?.total, online = counted?.online ?? null
  const parts = [cur?.name ?? currentHub().replace(/^https?:\/\//, ""), hubVersion && t("devices:stat.hubVersion", { version: hubVersion }),
    live.length > 1 ? t("devices:nodesN", { count: live.length }) : live.length ? t("devices:onlyLocal") : "", online != null ? t("devices:clientsOnlineN", { count: online }) : ""].filter(Boolean)
  const activeTab: Tab = tab === "monitor" && !owner ? "overview" : tab
  const options: SegmentOption<Tab>[] = [
    { value: "overview", label: t("devices:tab.overview"), data: { "data-seg": "overview" } },
    { value: "hub", label: t("devices:tab.hub"), data: { "data-seg": "hub" } },
    { value: "nodes", label: t("devices:tab.nodes"), count: live.length || undefined, data: { "data-seg": "nodes", "data-warn": warn ? "" : undefined } },
    { value: "clients", label: t("devices:tab.clients"), count: clients || undefined, data: { "data-seg": "clients" } },
    ...(owner ? [{
      value: "monitor" as const,
      label: t("devices:tab.monitor"),
      count: healthWarns.length || undefined,
      data: {
        "data-seg": "monitor",
        "data-bad": healthCritical ? "" : undefined,
        "data-warn": !healthCritical && healthWarns.length ? "" : undefined,
      },
    }] : []),
  ]
  return <div className="device-page"><Page title={t("devices:title")} max={1040} toolbarTop
    sub={<span className="inline-flex flex-wrap items-center gap-x-2 gap-y-0.5" data-devices-sub><NetworkIcon className="size-3.5 text-tertiary" aria-hidden />{parts.map((p, i, all) => <span key={i} className="inline-flex items-center gap-2"><span>{p}</span>{i < all.length - 1 && <span className="text-tertiary" aria-hidden>·</span>}</span>)}</span>}
    toolbar={<Button onClick={() => openSheet("addDevice")} data-add-device-open><PlusIcon />{t("devices:addDevice")}</Button>}>
    <SegmentedControl kind="tabs" variant="tray" size="md" aria-label={t("devices:tabs")} className="steve-seg mb-6" value={activeTab} onChange={(v) => go(devicesHref(v))}
      data={{ "data-devices-tabs": "" }}
      options={options} />
    {activeTab === "hub" ? <HubTab /> : activeTab === "nodes" ? <NodesTab /> : activeTab === "clients" ? <ClientsTab focus={focus} /> : activeTab === "monitor" ? <MonitorTab /> : <OverviewTab />}
  </Page></div>
}
