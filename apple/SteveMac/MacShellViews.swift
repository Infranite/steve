import SteveClientKit
import SwiftUI

/// Only the trusted page can identify the workspace currently on screen. The native state
/// endpoint's default workspace is intentionally never used as a fallback here.
@MainActor
enum MacShellContext {
    static func confirmed(_ coordinator: SteveClientCoordinator) -> NativeShellContext? {
        guard let context = coordinator.shellContext,
              context.hubID == coordinator.hubStore.activeHubID,
              context.ready, context.authenticated else { return nil }
        return context
    }

    static func title(_ coordinator: SteveClientCoordinator) -> String {
        let title = confirmed(coordinator)?.title.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return title.isEmpty ? "Steve" : title
    }

    static func workspace(_ coordinator: SteveClientCoordinator) -> String {
        guard let context = confirmed(coordinator), let id = context.workspaceID, !id.isEmpty else {
            return L10n.tr("工作区尚未确认", "Workspace not yet confirmed")
        }
        let name = context.workspaceName?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return name.isEmpty ? L10n.tr("当前工作区", "Current workspace") : name
    }
}

struct MacHubMenu: View {
    @ObservedObject var coordinator: SteveClientCoordinator
    @ObservedObject private var hubStore: HubStore
    @Environment(\.openWindow) private var openWindow

    init(coordinator: SteveClientCoordinator) {
        self.coordinator = coordinator
        self.hubStore = coordinator.hubStore
    }

    var body: some View {
        Menu {
            ForEach(hubStore.profiles) { profile in
                Button {
                    coordinator.switchToHub(id: profile.id)
                } label: {
                    if profile.id == hubStore.activeHubID {
                        Label(profile.displayName, systemImage: "checkmark")
                    } else {
                        Text(profile.displayName)
                    }
                }
            }
            Divider()
            Button(L10n.tr("添加或管理 Hub…", "Add or Manage Hubs…")) { navigate("#/hub/add") }
            Button(L10n.tr("配对新设备…", "Pair Device…")) { navigate("#/hub/pair") }
            Button(L10n.tr("设备…", "Devices…")) { navigate("#/devices") }
        } label: {
            Label {
                Text(hubStore.activeProfile.displayName)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(maxWidth: 170)
            } icon: {
                Image(systemName: "server.rack")
            }
        }
        .help(L10n.tr("切换或管理 Hub", "Switch or manage Hubs"))
        .accessibilityIdentifier("shell.hub")
    }

    private func navigate(_ route: String) {
        MacMainWindow.navigate(route, coordinator: coordinator, open: { openWindow(id: "main") })
    }
}

struct MacConnectionLabel: View {
    let phase: HubConnectionPhase

    var body: some View {
        Label(text, systemImage: symbol)
            .foregroundStyle(phase == .connected ? Color.secondary : Color.orange)
    }

    private var symbol: String {
        switch phase {
        case .connected: return "network"
        case .connecting: return "arrow.triangle.2.circlepath"
        case .authRequired: return "key"
        case .offline: return "wifi.slash"
        }
    }

    private var text: String {
        switch phase {
        case .connected: return L10n.tr("Hub 已连接", "Hub connected")
        case .connecting: return L10n.tr("正在连接 Hub", "Connecting to Hub")
        case .authRequired: return L10n.tr("Hub 需要登录", "Hub sign-in required")
        case .offline: return L10n.tr("Hub 离线", "Hub offline")
        }
    }
}

struct MacConnectionDetails: View {
    @ObservedObject var coordinator: SteveClientCoordinator
    @ObservedObject private var hubStore: HubStore

    init(coordinator: SteveClientCoordinator) {
        self.coordinator = coordinator
        self.hubStore = coordinator.hubStore
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            MacConnectionLabel(phase: coordinator.summary.phase)
                .font(.headline)
            LabeledContent(L10n.tr("Hub", "Hub"), value: hubStore.activeProfile.displayName)
            LabeledContent(L10n.tr("地址", "Address")) {
                Text(endpoint).textSelection(.enabled).multilineTextAlignment(.trailing)
            }
            LabeledContent(L10n.tr("连接路由", "Connection route"), value: coordinator.summary.route.displayTitle)
            if let version = coordinator.summary.hubVersion {
                LabeledContent(L10n.tr("Hub 版本", "Hub version"), value: version)
            }
            LabeledContent(L10n.tr("状态检查", "Status check")) {
                Text(coordinator.summary.updatedAt, style: .time)
            }
            if let error = coordinator.summary.errorMessage {
                Text(error).font(.callout).foregroundStyle(.secondary).textSelection(.enabled)
            }
            if let update = coordinator.summary.updateAvailable {
                Text(L10n.tr("Hub 有可用更新：\(update)", "Hub update available: \(update)"))
                    .font(.callout)
            }
            Button(L10n.tr("重新连接", "Reconnect")) { coordinator.reconnectActiveHub() }
                .accessibilityIdentifier("shell.reconnect")
        }
        .font(.callout)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var endpoint: String {
        coordinator.summary.activeEndpoint.map(URLNormalizer.canonicalOrigin)
            ?? hubStore.activeProfile.primaryDisplayAddress
    }
}

/// No personal reminder contract is available in this foundation release. A missing source is
/// not a known zero: there are no unread badges, aggregate totals, or synthetic recent entries.
struct MacAttentionView: View {
    @ObservedObject var coordinator: SteveClientCoordinator
    let navigate: (String) -> Void

    private enum Category: String, CaseIterable, Identifiable {
        case replies, pending, completions, channels
        var id: String { rawValue }
        var title: String {
            switch self {
            case .replies: return L10n.tr("Steve 回复", "Steve replies")
            case .pending: return L10n.tr("待处理", "Pending")
            case .completions: return L10n.tr("完成与交付", "Completions & deliveries")
            case .channels: return L10n.tr("渠道消息", "Channel messages")
            }
        }
        var symbol: String {
            switch self {
            case .replies: return "bubble.left"
            case .pending: return "tray"
            case .completions: return "checkmark.circle"
            case .channels: return "bubble.left.and.bubble.right"
            }
        }
        var route: String {
            switch self {
            case .replies: return "#/"
            case .pending: return "#/inbox"
            case .completions: return "#/feed"
            case .channels: return "#/channels"
            }
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(L10n.tr("提醒", "Attention")).font(.headline)
            Text(MacShellContext.workspace(coordinator))
                .font(.callout).foregroundStyle(.secondary)
                .accessibilityIdentifier("shell.attention.workspace")
            Text(L10n.tr(
                "个人提醒尚未接入。下方入口可查看原始内容；不可用不代表没有新消息。",
                "Personal reminders are not connected yet. Open the source pages below; unavailable does not mean there are no new messages."
            ))
            .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            ForEach(Category.allCases) { category in
                Button { navigate(category.route) } label: {
                    HStack(spacing: 10) {
                        Image(systemName: category.symbol).frame(width: 20)
                            .foregroundStyle(.secondary)
                        VStack(alignment: .leading, spacing: 3) {
                            Text(category.title).foregroundStyle(.primary)
                            Text(L10n.tr("提醒不可用 · 打开页面", "Reminders unavailable · Open page"))
                                .font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer(minLength: 4)
                        Image(systemName: "chevron.right").font(.caption).foregroundStyle(.tertiary)
                    }
                    .padding(.vertical, 5)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("shell.attention.\(category.rawValue)")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct MacClientSettingsView: View {
    var body: some View {
        Form {
            Section(L10n.tr("提醒与隐私", "Reminders & Privacy")) {
                Text(L10n.tr("此版本尚不发送系统通知，也不请求通知权限。", "This version does not send system notifications or request notification permission."))
                Text(L10n.tr(
                    "Steve 回复、待处理、完成与交付和渠道消息需接入个人提醒服务后才会显示准确状态。当前可从提醒面板打开对应页面。",
                    "Steve replies, pending actions, completions and channel messages need a personal reminder service before accurate status can be shown. The attention panel currently opens their source pages."
                ))
                .foregroundStyle(.secondary)
            }
            Section(L10n.tr("窗口与菜单栏", "Window & Menu Bar")) {
                Text(L10n.tr("关闭窗口（⌘W）会保留菜单栏客户端；点按 Dock 图标或“打开 Steve”可恢复窗口。", "Close the window (⌘W) to keep the menu bar client running. Click the Dock icon or Open Steve to restore it."))
                Text(L10n.tr("退出 Steve（⌘Q）只退出此客户端，不会停止 Hub。当前版本的后台连接检查不等于后台消息提醒。", "Quit Steve (⌘Q) exits only this client, not the Hub. Background connection checks in this version are not background message reminders."))
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .padding(8)
        .frame(width: 480)
        .fixedSize(horizontal: false, vertical: true)
    }
}

/// No new global composer shortcuts: Cmd-K, Cmd-B and Return remain owned by the web editor.
struct MacNavigationCommands: Commands {
    @ObservedObject var coordinator: SteveClientCoordinator
    @Environment(\.openWindow) private var openWindow

    var body: some Commands {
        CommandMenu(L10n.tr("导航", "Navigate")) {
            Button(L10n.tr("打开 Steve", "Open Steve")) {
                MacMainWindow.bringUpMainWindow(open: { openWindow(id: "main") })
            }
            Divider()
            Button(L10n.tr("对话", "Conversation")) { navigate("#/") }
            Button(L10n.tr("待处理", "Inbox")) { navigate("#/inbox") }
            Button(L10n.tr("动态与交付", "Feed & deliveries")) { navigate("#/feed") }
            Button(L10n.tr("渠道", "Channels")) { navigate("#/channels") }
            Button(L10n.tr("Hub 设置…", "Hub Settings…")) { navigate("#/devices/hub") }
        }
    }

    private func navigate(_ route: String) {
        MacMainWindow.navigate(route, coordinator: coordinator, open: { openWindow(id: "main") })
    }
}
