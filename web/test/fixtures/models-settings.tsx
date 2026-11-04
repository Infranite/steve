import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { Toaster } from "sonner"
import "../../src/manage" // the app loads manage first (it imports this pane back)
import { ModelsSettingsPane } from "../../src/models-settings"
document.documentElement.classList.toggle("dark", matchMedia("(prefers-color-scheme: dark)").matches)
createRoot(document.getElementById("root")!).render(<main className="mx-auto max-w-[640px] px-4 py-6"><h2 className="mb-5 type-title">模型</h2><ModelsSettingsPane /><Toaster /></main>)
