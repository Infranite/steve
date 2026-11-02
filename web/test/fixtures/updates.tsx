import { StrictMode, useState } from "react"
import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { AboutVersions } from "../../src/version"
import { st } from "../../src/store"
import { draftKey, readDraft, saveDraft } from "../../src/composer-draft"
st.version = "fixture-hub-version"
st.webVersion = ""
function Fixture() {
  const key = draftKey("test", "chat", "upgrade-draft")
  const [text, setText] = useState(() => readDraft(key).text)
  return <main style={{ maxWidth: 760, margin: "auto", padding: 20 }}>
    <label>对话草稿<textarea aria-label="对话草稿" value={text} onChange={e => { setText(e.target.value); saveDraft(key, { text: e.target.value, files: [], mentions: [] }) }} /></label>
    <AboutVersions />
  </main>
}
createRoot(document.getElementById("root")!).render(<StrictMode><Fixture /></StrictMode>)
