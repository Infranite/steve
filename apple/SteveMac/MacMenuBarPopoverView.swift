import AppKit
import SteveClientKit
import SwiftUI

struct MacMenuBarPopoverView: View {
    @ObservedObject var coordinator: SteveClientCoordinator
    @ObservedObject var attention: MacAttentionController
    @ObservedObject private var hubStore: HubStore
    @Environment(\.openWindow) private var openWindow
    @State private var showConnection = false

    init(coordinator: SteveClientCoordinator, attention: MacAttentionController) {
        self.coordinator = coordinator
        self.attention = attention
        self.hubStore = coordinator.hubStore
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text(hubStore.activeProfile.displayName).font(.headline).lineLimit(1)
                    Text(MacShellContext.title(coordinator)).font(.callout).lineLimit(1)
                }
                Spacer()
                Button { showConnection.toggle() } label: { Image(systemName: "network") }
                    .help(L10n.tr("连接详情", "Connection details"))
                    .popover(isPresented: $showConnection) {
                        MacConnectionDetails(coordinator: coordinator).padding(16).frame(width: 350)
                    }
            }
            MacConnectionLabel(phase: coordinator.summary.phase).font(.caption)
            Divider()
            MacAttentionView(coordinator: coordinator, attention: attention, navigate: navigate)
            if let error = coordinator.shellNavigationError {
                Text(error).font(.caption).foregroundStyle(.orange)
                Button(L10n.tr("关闭导航错误", "Dismiss navigation error")) { coordinator.dismissShellNavigationError() }
            }
            Divider()
            MacHubMenu(coordinator: coordinator)
            HStack {
                Button(L10n.tr("打开 Steve", "Open Steve")) { MacMainWindow.bringUpMainWindow(open: { openWindow(id: "main") }) }
                    .buttonStyle(.borderedProminent)
                Spacer()
                SettingsLink { Image(systemName: "gearshape") }
                    .help(L10n.tr("设置…", "Settings…"))
                Button(L10n.tr("退出", "Quit")) { NSApplication.shared.terminate(nil) }
            }
        }
        .padding(16)
        .frame(width: 340)
    }

    private func navigate(_ route: String) {
        MacMainWindow.navigate(route, coordinator: coordinator, open: { openWindow(id: "main") })
    }
}
