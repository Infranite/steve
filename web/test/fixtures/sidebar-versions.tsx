import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { st, useStore } from "../../src/store"
import { Sidebar } from "../../src/shell"
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
/* the Hub runs another build than this page: both are named at the bottom of the sidebar (ST-21) */
st.ready = true
st.version = new URLSearchParams(location.search).get("hub") ?? "0.2.53"
function App() { useStore(); return <SidebarProvider><Sidebar route="chat" /><main className="w-full p-6">正文</main></SidebarProvider> }
createRoot(document.getElementById("root")!).render(<App />)
