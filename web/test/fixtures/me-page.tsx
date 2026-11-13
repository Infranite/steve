import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { ContactView } from "../../src/people/contact"
createRoot(document.getElementById("root")!).render(<SidebarProvider><main className="h-svh w-full">{<ContactView id={new URLSearchParams(location.search).get("id") ?? "me"} />}</main></SidebarProvider>)
