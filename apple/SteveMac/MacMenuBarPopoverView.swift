import AppKit
import SteveClientKit
import SwiftUI

struct MacMenuBarPopoverView: View {
    @ObservedObject var coordinator: SteveClientCoordinator
    @ObservedObject private var hubStore: HubStore
    @Environment(\.openWindow) private var openWindow

    init(coordinator: SteveClientCoordinator) {
        self.coordinator = coordinator
        self.hubStore = coordinator.hubStore
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            headerSection
            Divider()
            metricsGrid
            if let update = coordinator.summary.updateAvailable {
                updateBanner(version: update)
            }
            Divider()
            hubQuickSwitchSection
            Divider()
            actionButtonsSection
        }
        .padding(14)
        .frame(width: 320)
    }

    private var headerSection: some View {
        HStack(alignment: .top, spacing: 10) {
            ZStack {
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .fill(statusColor.opacity(0.14))
                    .frame(width: 36, height: 36)
                Image(systemName: statusIcon)
                    .font(.system(size: 16, weight: .semibold))
                    .foregroundStyle(statusColor)
            }

            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(hubStore.activeProfile.name)
                        .font(.headline)
                        .lineLimit(1)
                    Text(statusText)
                        .font(.caption2.weight(.medium))
                        .padding(.horizontal, 6)
                        .padding(.vertical, 1.5)
                        .background(statusColor.opacity(0.16), in: Capsule())
                        .foregroundStyle(statusColor)
                }

                Text(endpointLine)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .lineLimit(1)

                HStack(spacing: 8) {
                    Label(coordinator.summary.route.displayTitle, systemImage: routeSymbol)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                    if let version = coordinator.summary.hubVersion {
                        Text("v\(version.trimmingCharacters(in: CharacterSet(charactersIn: "v")))")
                            .font(.caption2.monospaced())
                            .foregroundStyle(.secondary)
                    }
                    if let ws = coordinator.summary.workspaceName, !ws.isEmpty {
                        Text("· \(ws)")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                    }
                }
            }

            Spacer(minLength: 0)

            Button {
                coordinator.reconnectActiveHub()
            } label: {
                Image(systemName: "arrow.clockwise")
                    .font(.caption)
            }
            .buttonStyle(.borderless)
            .help(L10n.tr("刷新 Hub 状态", "Refresh Hub status"))
        }
    }

    private var metricsGrid: some View {
        let summary = coordinator.summary
        return VStack(spacing: 8) {
            HStack(spacing: 8) {
                MetricTile(
                    title: L10n.tr("运行中会话", "Active Sessions"),
                    value: summary.phase == .connected ? "\(summary.runningSessions)" : "—",
                    subtitle: summary.steveThinking ? L10n.tr("Steve 正在思考", "Steve is thinking") : L10n.tr("空闲待命", "Idle"),
                    icon: "terminal",
                    tint: summary.runningSessions > 0 || summary.steveThinking ? .blue : .secondary
                )
                MetricTile(
                    title: L10n.tr("待确认事项", "Open Decisions"),
                    value: summary.phase == .connected ? "\(summary.openDecisions)" : "—",
                    subtitle: summary.openDecisions > 0 ? L10n.tr("等待你的拍板", "Awaiting decision") : L10n.tr("全部已处理", "All clear"),
                    icon: "tray.full",
                    tint: summary.openDecisions > 0 ? .orange : .secondary,
                    onTap: summary.openDecisions > 0 ? {
                        activateMainWindow(route: "#/inbox")
                    } : nil
                )
            }

            HStack(spacing: 8) {
                MetricTile(
                    title: L10n.tr("在线设备 / Node", "Online Nodes"),
                    value: summary.phase == .connected ? "\(summary.onlineDevices)/\(max(summary.totalDevices, summary.onlineDevices))" : "—",
                    subtitle: summary.onlineDevices > 0 ? L10n.tr("设备健康在线", "Healthy") : L10n.tr("未连接执行节点", "No nodes online"),
                    icon: "cpu",
                    tint: summary.onlineDevices > 0 ? .green : .secondary,
                    onTap: {
                        activateMainWindow(route: "#/devices")
                    }
                )
                MetricTile(
                    title: L10n.tr("连接路由", "Route"),
                    value: summary.route.displayTitle,
                    subtitle: hubStore.activeProfile.publicURL != nil ? L10n.tr("公网默认", "Public default") : L10n.tr("内网 / 本机", "LAN / Local"),
                    icon: routeSymbol,
                    tint: .primary
                )
            }
        }
    }

    private func updateBanner(version: String) -> some View {
        Button {
            activateMainWindow(route: "#/settings/about")
        } label: {
            HStack(spacing: 8) {
                Image(systemName: "arrow.up.circle.fill")
                    .foregroundStyle(.blue)
                Text(L10n.tr("发现新版本 \(version)，点击查看升级", "Update \(version) available — click to view"))
                    .font(.caption)
                    .foregroundStyle(.primary)
                Spacer()
                Image(systemName: "chevron.right")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            .background(Color.blue.opacity(0.1), in: RoundedRectangle(cornerRadius: 8))
        }
        .buttonStyle(.plain)
    }

    private var hubQuickSwitchSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text(L10n.tr("切换 Hub", "Switch Hub"))
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                Spacer()
                Button(L10n.tr("添加 / 管理…", "Manage…")) {
                    activateMainWindow(route: nil)
                    coordinator.isHubSwitcherPresented = true
                }
                .buttonStyle(.link)
                .font(.caption)
            }

            ForEach(hubStore.profiles.prefix(5)) { profile in
                Button {
                    coordinator.switchToHub(id: profile.id)
                } label: {
                    HStack(spacing: 8) {
                        Image(systemName: profile.id == hubStore.activeHubID ? "largecircle.fill.circle" : "circle")
                            .font(.caption)
                            .foregroundStyle(profile.id == hubStore.activeHubID ? Color.accentColor : .secondary)
                        Text(profile.name)
                            .font(.callout)
                            .lineLimit(1)
                        Spacer()
                        Text(profile.isLocalDefault ? L10n.tr("本机", "Local") : (profile.publicURL != nil ? L10n.tr("公网", "Public") : L10n.tr("内网", "LAN")))
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                    .padding(.vertical, 3)
                    .padding(.horizontal, 6)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
    }

    private var actionButtonsSection: some View {
        VStack(spacing: 6) {
            HStack(spacing: 8) {
                Button {
                    activateMainWindow(route: nil)
                } label: {
                    Label(L10n.tr("打开主窗口", "Open Window"), systemImage: "macwindow")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.regular)

                Button {
                    activateMainWindow(route: nil)
                    coordinator.isPairQRPresented = true
                } label: {
                    Label(L10n.tr("手机扫码配对", "Pair Phone"), systemImage: "qrcode")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
                .controlSize(.regular)
            }

            HStack {
                Button(L10n.tr("设置 · 访问与客户端", "Settings · Access")) {
                    activateMainWindow(route: "#/settings/access")
                }
                .buttonStyle(.borderless)
                .font(.caption)
                .foregroundStyle(.secondary)

                Spacer()

                Button(L10n.tr("退出 Steve", "Quit Steve")) {
                    NSApplication.shared.terminate(nil)
                }
                .buttonStyle(.borderless)
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            .padding(.top, 2)
        }
    }

    private func activateMainWindow(route: String?) {
        openWindow(id: "main")
        NSApplication.shared.activate(ignoringOtherApps: true)
        if let route {
            coordinator.navigate(hashRoute: route)
        }
    }

    private var endpointLine: String {
        if let active = coordinator.summary.activeEndpoint {
            return URLNormalizer.canonicalOrigin(active)
        }
        return hubStore.activeProfile.primaryDisplayAddress
    }

    private var statusColor: Color {
        switch coordinator.summary.phase {
        case .connected: return .green
        case .connecting: return .orange
        case .authRequired: return .yellow
        case .offline: return .red
        }
    }

    private var statusIcon: String {
        switch coordinator.summary.phase {
        case .connected: return "checkmark.shield.fill"
        case .connecting: return "arrow.triangle.2.circlepath"
        case .authRequired: return "key.fill"
        case .offline: return "wifi.slash"
        }
    }

    private var statusText: String {
        switch coordinator.summary.phase {
        case .connected: return L10n.tr("已连接", "Connected")
        case .connecting: return L10n.tr("连接中", "Connecting")
        case .authRequired: return L10n.tr("待登录", "Sign in")
        case .offline: return L10n.tr("离线", "Offline")
        }
    }

    private var routeSymbol: String {
        switch coordinator.summary.route {
        case .publicNetwork: return "globe"
        case .lan: return "wifi"
        case .tailnet: return "point.3.connected.trianglepath.dotted"
        case .loopback: return "laptopcomputer"
        case .proxy: return "arrow.left.arrow.right"
        case .unknown: return "network"
        }
    }
}

private struct MetricTile: View {
    let title: String
    let value: String
    let subtitle: String
    let icon: String
    let tint: Color
    var onTap: (() -> Void)? = nil

    var body: some View {
        Group {
            if let onTap {
                Button(action: onTap) { tileBody }
                    .buttonStyle(.plain)
            } else {
                tileBody
            }
        }
    }

    private var tileBody: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 4) {
                Image(systemName: icon)
                    .font(.caption2)
                    .foregroundStyle(tint)
                Text(title)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Text(value)
                .font(.system(.callout, design: .rounded).weight(.semibold))
                .monospacedDigit()
                .lineLimit(1)
            Text(subtitle)
                .font(.system(size: 10))
                .foregroundStyle(.secondary)
                .lineLimit(1)
        }
        .padding(8)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 9, style: .continuous))
    }
}
