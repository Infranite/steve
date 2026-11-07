import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import { Toaster } from "sonner"
import "../../src/index.css"
import { WorkspaceDisk } from "../../src/disk-settings"
import { OutboxDead } from "../../src/outbox-settings"
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
createRoot(document.getElementById("root")!).render(<StrictMode><main style={{ maxWidth: 640, margin: "auto", padding: 20 }}><WorkspaceDisk /><div style={{ height: 24 }} /><OutboxDead /></main><Toaster /></StrictMode>)
