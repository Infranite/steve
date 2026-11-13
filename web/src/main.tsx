import { StrictMode } from "react"
import { createRoot } from "react-dom/client"
import "./i18n/index.ts"
import "@fontsource-variable/geist"
import "@fontsource-variable/geist-mono"
import "./index.css"
import App from "./App.tsx"
import { applyStoredLook } from "@/lib/motion"

/* the type size and reduced motion this device saw last, until the Hub's settings arrive */
applyStoredLook()

createRoot(document.getElementById("root")!).render(<StrictMode><App /></StrictMode>)
