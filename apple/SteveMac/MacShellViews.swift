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
    @ObservedObject private var language = NativeUILanguageState.shared
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
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .frame(maxWidth: 120)
            } icon: {
                Image(systemName: "server.rack").foregroundStyle(.secondary)
            }
        }
        .labelStyle(.titleAndIcon)
        .menuStyle(.borderlessButton)
        .help(L10n.tr("切换或管理 Hub", "Switch or manage Hubs"))
        .accessibilityIdentifier("shell.hub")
    }

    private func navigate(_ route: String) {
        MacMainWindow.navigate(route, coordinator: coordinator, open: { openWindow(id: "main") })
    }
}

extension ToolbarContent {
    @ToolbarContentBuilder
    func macUnsharedBackground() -> some ToolbarContent {
        #if compiler(>=6.2)
        if #available(macOS 26.0, *) {
            self.sharedBackgroundVisibility(.hidden)
        } else {
            self
        }
        #else
        self
        #endif
    }
}

struct MacConnectionIndicator: View {
    @ObservedObject private var language = NativeUILanguageState.shared
    let phase: HubConnectionPhase

    var body: some View {
        switch phase {
        case .connected:
            Image(systemName: "circle.fill")
                .font(.system(size: 6))
                .foregroundStyle(.secondary)
                .frame(width: 20, height: 20)
                .accessibilityLabel(L10n.tr("Hub 已连接", "Hub connected"))
        default:
            MacConnectionLabel(phase: phase)
        }
    }
}

struct MacConnectionLabel: View {
    @ObservedObject private var language = NativeUILanguageState.shared
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
    @ObservedObject private var language = NativeUILanguageState.shared
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

/// Separate category units, never an aggregate count. Missing/stale sources are not a known zero.
struct MacAttentionView: View {
    @ObservedObject private var language = NativeUILanguageState.shared
    @ObservedObject var coordinator: SteveClientCoordinator
    @ObservedObject var attention: MacAttentionController
    let navigate: (String) -> Void

    private enum Category: String, CaseIterable, Identifiable {
        case replies, pending, completions, channels
        var id: String { rawValue }
        var kind: AttentionKind { self == .completions ? .feed : AttentionKind(rawValue: rawValue)! }
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
            Text(attention.workspaceName ?? MacShellContext.workspace(coordinator))
                .font(.callout).foregroundStyle(.secondary)
                .accessibilityIdentifier("shell.attention.workspace")
            Text(L10n.tr("回复按消息、待处理按卡片、交付按条目、渠道按会话计数，不合并相加。", "Replies count messages, pending counts cards, deliveries count entries, and channels count conversations. Counts are not added together."))
                .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            if let error = attention.error { Text(error).font(.caption).foregroundStyle(.orange) }
            ForEach(Category.allCases) { category in
                Button { navigate(category.route) } label: {
                    HStack(spacing: 10) {
                        Image(systemName: category.symbol).frame(width: 20)
                            .foregroundStyle(.secondary)
                        VStack(alignment: .leading, spacing: 3) {
                            Text(category.title).foregroundStyle(.primary)
                            Text(status(category.kind))
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
                if let scope = attention.state.scope,
                   let entry = attention.state.categories[category.kind]?.lastSuccess?.entries.max(by: { $0.created < $1.created }) {
                    Button(L10n.tr("打开最新条目", "Open latest entry")) { attention.open(scope: scope, target: entry.target) }
                        .font(.caption)
                }
            }
            if let updated = attention.state.updatedAt {
                Text(updated, style: .time).font(.caption2).foregroundStyle(.secondary)
            }
            Button(L10n.tr("重新检查", "Check again")) { attention.retry() }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func status(_ kind: AttentionKind) -> String {
        guard let category = attention.state.categories[kind] else { return L10n.tr("尚未确认 · 需要登录和工作区", "Not confirmed · Sign in and select a workspace") }
        let count = category.lastSuccess.map { value -> String in
            let n = value.count
            switch kind {
            case .replies: return L10n.tr("\(n) 条消息", "\(n) \(n == 1 ? "message" : "messages")")
            case .pending: return L10n.tr("\(n) 张卡片", "\(n) \(n == 1 ? "card" : "cards")")
            case .feed: return L10n.tr("\(n) 条交付", "\(n) \(n == 1 ? "entry" : "entries")")
            case .channels: return L10n.tr("\(n) 个会话", "\(n) \(n == 1 ? "conversation" : "conversations")")
            }
        }
        switch category.phase {
        case .known: return count ?? L10n.tr("未知", "Unknown")
        case .loading: return L10n.tr("正在检查…", "Checking…")
        case .stale: return L10n.tr("连接中断 · 上次：", "Connection unavailable · Last: ") + (count ?? L10n.tr("未知", "Unknown"))
        case .unsupported: return L10n.tr("此 Hub 不支持", "Unsupported by this Hub")
        case .authRequired: return L10n.tr("需要重新登录", "Sign-in required")
        }
    }
}

struct MacClientSettingsView: View {
    @ObservedObject private var language = NativeUILanguageState.shared
    @ObservedObject var attention: MacAttentionController
    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if let error = attention.error { Text(error).foregroundStyle(.orange).padding(.horizontal) }
            if attention.state.scope == nil {
                Text(L10n.tr("请先连接 Hub 并确认账号和工作区。", "Connect a Hub and confirm your account and workspace first.")).padding()
            }
            MacAttentionSettingsView(delivery: attention.delivery,
                preferences: Binding(get: { attention.preferences }, set: { attention.setPreferences($0) }),
                dedupStatus: attention.state.dedupStatus,
                channels: attention.state.categories[.channels]?.lastSuccess?.entries.map(\.target.entityID) ?? [])
                .disabled(attention.state.scope == nil)
            Text(L10n.tr("关闭窗口后继续提醒；⌘Q 只退出客户端，不停止 Hub。", "Reminders continue with the window closed. ⌘Q quits only the client, not the Hub."))
                .font(.caption).foregroundStyle(.secondary).padding()
        }
        .frame(width: 540, height: 660)
    }
}

/// No new global composer shortcuts: Cmd-K, Cmd-B and Return remain owned by the web editor.
struct MacNavigationCommands: Commands {
    @ObservedObject private var language = NativeUILanguageState.shared
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
