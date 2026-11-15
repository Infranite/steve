import { createRoot } from "react-dom/client"
import "../../src/index.css"
import { SidebarProvider } from "../../src/components/ui/sidebar"
import { PendingContacts } from "../../src/people/pending"
import { OrgView } from "../../src/people/org"
import { ContactView } from "../../src/people/contact"
import { ProposalCard } from "../../src/config-ui"
const proposal = { state: "draft", title: "Update profile", changes: [
  { label: "Project", from: "(empty)", to: "Release tooling" },
  { label: "Obsolete fact", from: "Old role", to: "(empty)" },
  { label: "Original note", from: "I work on reliable release tooling. ".repeat(9), to: "I work on portable reliable release tooling. ".repeat(9) },
], apply: async () => {}, drop: async () => {}, undo: async () => {} }
createRoot(document.getElementById("root")!).render(<SidebarProvider><main className="mx-auto h-svh w-full max-w-3xl overflow-hidden">{location.search.includes("proposal") ? <ProposalCard p={proposal} /> : location.search.includes("me") ? <ContactView id="me" /> : location.search.includes("org") ? <OrgView id="o1" /> : location.search.includes("contact") ? <ContactView id="c1" /> : <PendingContacts />}</main></SidebarProvider>)
