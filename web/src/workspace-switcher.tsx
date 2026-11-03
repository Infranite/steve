import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { SidebarMenuButton, SidebarMenuItem } from "@/components/ui/sidebar"
import { st, switchWorkspace } from "@/store"
import { go, openSheet } from "@/ui"
import { CheckIcon, ChevronsUpDownIcon, LayersIcon, PlusIcon, SettingsIcon } from "lucide-react"
export function WorkspaceSwitcher() {
  const w = st.workspace
  if (!w) return null
  return (
    <SidebarMenuItem>
      <DropdownMenu>
        <DropdownMenuTrigger render={<SidebarMenuButton tooltip={"工作空间 · " + w.name} className="text-muted-foreground" data-ws-switch />}>
          <LayersIcon className="size-4! shrink-0" strokeWidth={1.5} />
          <span className="min-w-0 flex-1 truncate text-foreground/85">{w.name}</span><ChevronsUpDownIcon className="size-3.5! opacity-60" />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="min-w-56">
          {/* a label must sit inside a group: outside one, Base UI throws and blanks the page */}
          <DropdownMenuGroup>
            <DropdownMenuLabel className="type-footnote text-muted-foreground">工作空间</DropdownMenuLabel>
            {st.workspaces.map((x) => (
              <DropdownMenuItem key={x.id} onClick={() => x.id !== w.id && void switchWorkspace(x.id)} data-ws-opt={x.name}>
                <LayersIcon className="size-4 text-muted-foreground" strokeWidth={1.5} /><span className="flex-1">{x.name}</span>{x.id === w.id && <CheckIcon className="size-4 text-blue" />}
              </DropdownMenuItem>))}
          </DropdownMenuGroup>
          <DropdownMenuSeparator />
          <DropdownMenuItem onClick={() => openSheet("newWorkspace")} data-new-ws><PlusIcon />新建工作空间</DropdownMenuItem>
          <DropdownMenuItem onClick={() => go("#/settings/workspace")}><SettingsIcon />管理这个工作空间</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </SidebarMenuItem>
  )
}