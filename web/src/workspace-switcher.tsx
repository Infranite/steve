import { DropdownMenu, DropdownMenuContent, DropdownMenuGroup, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu"
import { SidebarMenuButton, SidebarMenuItem } from "@/components/ui/sidebar"
import { st, switchWorkspace } from "@/store"
import { BotAvatar, go, openSheet } from "@/ui"
import { useT } from "@/i18n"
import { CheckIcon, ChevronDownIcon, LayersIcon, PlusIcon, SettingsIcon } from "lucide-react"
export function WorkspaceSwitcher() {
  const w = st.workspace, t = useT()
  if (!w) return null
  return (
    <SidebarMenuItem>
      <DropdownMenu>
        {/* the sidebar's head, App Shell's workspace button: Steve's mark as the logo, the workspace's name, its menu */}
        <DropdownMenuTrigger render={<SidebarMenuButton tooltip={t("shell:space.tooltip", { name: w.name })} className="h-8 gap-2 rounded-lg px-1! group-data-[collapsible=icon]:p-1!" data-ws-switch />}>
          <BotAvatar k="steve" size={22} className="size-5.5! shrink-0 transition-opacity duration-120 group-data-[collapsible=icon]:group-hover/head:opacity-0 group-data-[collapsible=icon]:group-focus-within/head:opacity-0" />
          <span className="min-w-0 flex-1 truncate type-headline">{w.name}</span><ChevronDownIcon className="size-3.5! text-tertiary" />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className="min-w-56">
          {/* a label must sit inside a group: outside one, Base UI throws and blanks the page */}
          <DropdownMenuGroup>
            <DropdownMenuLabel className="type-label text-tertiary">{t("shell:space.label")}</DropdownMenuLabel>
            {st.workspaces.map((x) => (
              <DropdownMenuItem key={x.id} onClick={() => x.id !== w.id && void switchWorkspace(x.id)} data-ws-opt={x.name}>
                <LayersIcon className="size-4 text-muted-foreground" strokeWidth={1.5} /><span className="min-w-0 flex-1 truncate">{x.name}</span>{x.id === w.id && <CheckIcon className="size-4 text-blue" />}
              </DropdownMenuItem>))}
          </DropdownMenuGroup>
          <DropdownMenuSeparator />
          <DropdownMenuItem onClick={() => openSheet("newWorkspace")} data-new-ws><PlusIcon />{t("shell:space.new")}</DropdownMenuItem>
          <DropdownMenuItem onClick={() => go("#/settings/workspace")}><SettingsIcon />{t("shell:space.manage")}</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </SidebarMenuItem>
  )
}