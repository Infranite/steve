/* Devices: one page for the Hub in use and everything on it, in tabs: Overview (#/devices), the Hub
 * (#/devices/hub), its Nodes (#/devices/nodes, a Node at #/devices/nodes/<id>) and the clients signed in to it
 * (#/devices/clients, a client's row at #/devices/clients/<id>). Old places lead here: #/settings/hub, #/settings/access and #/devices/<device id>. */
import { useEffect } from "react"
import { NetworkIcon, PlusIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { SegmentedControl } from "@/components/affect/segmented-control-clean"
import { go, openSheet, Page } from "@/ui"
import { useStore, useDeviceInventory } from "@/store"
import { currentHub } from "@/hub"
import { useHubs } from "@/hub-ui"
import { useT } from "@/i18n"
import { nodeStatus } from "@/device-model"
import { mapClients } from "@/device-map-model"
import { DevicesProvider, useDevices } from "@/devices-data"
import { OverviewTab } from "@/devices-overview"
import { HubTab } from "@/devices-hub"
import { NodesTab } from "@/devices-nodes"
import { ClientsTab } from "@/devices-clients"
import { NodePage } from "@/devices-node"
import "./devices.css"

const TABS = ["overview", "hub", "nodes", "clients"] as const
type Tab = (typeof TABS)[number]
export const devicesHref = (tab: Tab) => (tab === "overview" ? "#/devices" : "#/devices/" + tab)

/** replaces the address in history (an old link must not be a step back) */
function Redirect({ to }: { to: string }) {
  useEffect(() => { history.replaceState(null, "", location.pathname + location.search + to); window.dispatchEvent(new HashChangeEvent("hashchange")) }, [to])
  return null
}

/** the route under #/devices: a tab, a Node's page, or an old device link sent to its Node */
export function DevicesRoute({ b, c }: { b?: string; c?: string }) {
  useStore()
  const redirected = !!b && !(TABS as readonly string[]).includes(b)
  const devices = useDeviceInventory(redirected)
  if (b && redirected) {
    const id = decodeURIComponent(b), d = devices.find((x) => x.id === id)
    return <Redirect to={"#/devices/nodes/" + encodeURIComponent(d?.node_id ?? id)} />
  }
  return <DevicesProvider>{b === "nodes" && c ? <NodePage key={c} id={decodeURIComponent(c)} /> : <DevicesPage tab={(b as Tab) ?? "overview"} focus={b === "clients" && c ? decodeURIComponent(c) : undefined} />}</DevicesProvider>
}

function DevicesPage({ tab, focus }: { tab: Tab; focus?: string }) {
  const t = useT(), { views, hubVersion, access } = useDevices(), { hubs } = useHubs()
  const cur = hubs?.find((h) => h.current)
  const live = views.filter((v) => v.state !== "revoked")
  const warn = live.filter((v) => ["warn", "bad"].includes(nodeStatus(v, hubVersion).tone)).length
  /* the clients as the overview and the map count them: one per client, this browser included even when its sign-in
   * is no registered client; the head says how many are online now, the tab how many are signed in */
  const counted = access ? mapClients(access, t("devices:clients.browserSession")) : null
  const clients = counted?.total, online = counted?.online ?? null
  const parts = [cur?.name ?? currentHub().replace(/^https?:\/\//, ""), hubVersion && t("devices:stat.hubVersion", { version: hubVersion }),
    live.length > 1 ? t("devices:nodesN", { count: live.length }) : live.length ? t("devices:onlyLocal") : "", online != null ? t("devices:clientsOnlineN", { count: online }) : ""].filter(Boolean)
  return <div className="device-page"><Page title={t("devices:title")} max={1040} toolbarTop
    sub={<span className="inline-flex flex-wrap items-center gap-x-2 gap-y-0.5" data-devices-sub><NetworkIcon className="size-3.5 text-tertiary" aria-hidden />{parts.map((p, i, all) => <span key={i} className="inline-flex items-center gap-2"><span>{p}</span>{i < all.length - 1 && <span className="text-tertiary" aria-hidden>·</span>}</span>)}</span>}
    toolbar={<Button onClick={() => openSheet("addDevice")} data-add-device-open><PlusIcon />{t("devices:addDevice")}</Button>}>
    <SegmentedControl kind="tabs" variant="tray" size="md" aria-label={t("devices:tabs")} className="steve-seg mb-6" value={tab} onChange={(v) => go(devicesHref(v))}
      data={{ "data-devices-tabs": "" }}
      options={[
        { value: "overview", label: t("devices:tab.overview"), data: { "data-seg": "overview" } },
        { value: "hub", label: t("devices:tab.hub"), data: { "data-seg": "hub" } },
        { value: "nodes", label: t("devices:tab.nodes"), count: live.length || undefined, data: { "data-seg": "nodes", "data-warn": warn ? "" : undefined } },
        { value: "clients", label: t("devices:tab.clients"), count: clients || undefined, data: { "data-seg": "clients" } },
      ]} />
    {tab === "hub" ? <HubTab /> : tab === "nodes" ? <NodesTab /> : tab === "clients" ? <ClientsTab focus={focus} /> : <OverviewTab />}
  </Page></div>
}
