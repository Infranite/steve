import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { PendingContacts } from "../../src/people/pending"
import { OrgView } from "../../src/people/org"
import { ContactView } from "../../src/people/contact"
createRoot(document.getElementById("root")!).render(<SidebarProvider><main className="mx-auto max-w-3xl p-4">{location.search.includes("org") ? <OrgView id="o1" /> : location.search.includes("contact") ? <ContactView id="c1" /> : <PendingContacts />}</main></SidebarProvider>)
