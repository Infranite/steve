import { createElement } from "react"
import { createRoot } from "react-dom/client"
import { getTranscript, useTranscript } from "../../src/data/transcript"
import { hubSubs, loadItem, start, useStore } from "../../src/store"

/* C1-S4: the store against a Hub the browser test plays (stream-resume.browser.mjs). The page keeps two item details and
 * one transcript loaded, and counts the full resyncs module pages are told about. */
const w = window as unknown as Record<string, unknown>
let resyncs = 0
hubSubs.add((e) => { if (e.type === "resync") resyncs++ })
start()
w.__resyncs = () => resyncs
w.__transcript = () => getTranscript("s1")?.turns.flatMap((turn) => turn.ev.map((e) => String(e[1]))) ?? []
function TranscriptWatcher() { useStore(); useTranscript("s1"); return null }
w.__load = async () => {
  await loadItem("ST-1"); await loadItem("ST-2")
  createRoot(document.getElementById("root")!).render(createElement(TranscriptWatcher))
}
