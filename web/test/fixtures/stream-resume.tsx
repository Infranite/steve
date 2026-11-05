import { hubSubs, loadEvents, loadItem, start, st } from "../../src/store"

/* C1-S4: the store against a Hub the browser test plays (stream-resume.browser.mjs). The page keeps two item details and
 * one transcript loaded, and counts the full resyncs module pages are told about. */
const w = window as unknown as Record<string, unknown>
let resyncs = 0
hubSubs.add((e) => { if (e.type === "resync") resyncs++ })
start()
w.__resyncs = () => resyncs
w.__transcript = () => (st.S.s1?.ev ?? []).map((e) => String(e[1]))
w.__load = async () => { await loadItem("ST-1"); await loadItem("ST-2"); await loadEvents("s1") }
