import SteveClientKit
import SwiftUI

struct MacMainWindowView: View {
    @ObservedObject var coordinator: SteveClientCoordinator
    @ObservedObject private var hubStore: HubStore

    init(coordinator: SteveClientCoordinator) {
        self.coordinator = coordinator
        self.hubStore = coordinator.hubStore
    }

    var body: some View {
        ZStack {
            if coordinator.localGatewayURL != nil {
                SteveWebView(coordinator: coordinator, platformName: "macos")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if let startupError = coordinator.proxyStartupError {
                VStack(spacing: 14) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.largeTitle)
                        .foregroundStyle(.orange)
                    Text(L10n.tr("本地网关启动失败", "Failed to start local gateway"))
                        .font(.headline)
                    Text(startupError)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .frame(maxWidth: 420)
                    Button(L10n.tr("重试", "Retry")) {
                        coordinator.retryGatewayStartup()
                    }
                    .buttonStyle(.borderedProminent)
                }
                .padding(24)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ProgressView(L10n.tr("正在启动 Steve 客户端…", "Starting Steve client…"))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }

            if coordinator.localGatewayURL != nil && coordinator.summary.phase == .offline {
                offlineBanner
            }
        }
        .navigationTitle(windowTitle)
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Menu {
                    Section(L10n.tr("快速切换 Hub", "Switch Hub")) {
                        ForEach(hubStore.profiles) { profile in
                            Button {
                                coordinator.switchToHub(id: profile.id)
                            } label: {
                                HStack {
                                    Text(profile.name)
                                    if profile.id == hubStore.activeHubID {
                                        Image(systemName: "checkmark")
                                    }
                                }
                            }
                        }
                    }
                    Divider()
                    Button {
                        coordinator.isHubSwitcherPresented = true
                    } label: {
                        Label(L10n.tr("管理与添加 Hub…", "Manage & Add Hubs…"), systemImage: "server.rack")
                    }
                    Button {
                        coordinator.reconnectActiveHub()
                    } label: {
                        Label(L10n.tr("重新连接", "Reconnect"), systemImage: "arrow.clockwise")
                    }
                } label: {
                    HStack(spacing: 6) {
                        Circle()
                            .fill(statusColor)
                            .frame(width: 7, height: 7)
                        Text(hubStore.activeProfile.name)
                            .font(.callout.weight(.medium))
                    }
                }
                .help(L10n.tr("当前连接的 Hub（点击切换或添加其它 Hub）", "Connected Hub (click to switch or add Hubs)"))

                Button {
                    coordinator.isPairQRPresented = true
                } label: {
                    Label(L10n.tr("手机配对", "Pair Phone"), systemImage: "qrcode")
                }
                .help(L10n.tr("生成配对二维码供 iOS 扫码连接", "Generate a QR code for iOS pairing"))
            }
        }
        .sheet(isPresented: $coordinator.isHubSwitcherPresented) {
            HubSwitcherSheet(coordinator: coordinator, allowsQRScan: false)
        }
        .sheet(isPresented: $coordinator.isPairQRPresented) {
            PairQRSheet(coordinator: coordinator)
        }
        .alert(
            L10n.tr("确认配对并连接到此 Hub？", "Pair and connect to this Hub?"),
            isPresented: Binding(
                get: { coordinator.pendingExternalPairConfirmation != nil },
                set: { if !$0 { coordinator.cancelPendingExternalPairing() } }
            ),
            presenting: coordinator.pendingExternalPairConfirmation
        ) { _ in
            Button(L10n.tr("取消", "Cancel"), role: .cancel) {
                coordinator.cancelPendingExternalPairing()
            }
            Button(L10n.tr("确认配对", "Pair")) {
                Task { await coordinator.confirmPendingExternalPairing() }
            }
        } message: { pending in
            Text(
                L10n.tr(
                    "名称：\(pending.displayName)\n目标地址：\n\(pending.endpointsSummary)",
                    "Name: \(pending.displayName)\nTarget endpoints:\n\(pending.endpointsSummary)"
                )
            )
        }
        .alert(
            L10n.tr("配对失败", "Pairing Failed"),
            isPresented: Binding(
                get: { coordinator.externalPairErrorMessage != nil },
                set: { if !$0 { coordinator.externalPairErrorMessage = nil } }
            )
        ) {
            Button(L10n.tr("好", "OK"), role: .cancel) {
                coordinator.externalPairErrorMessage = nil
            }
        } message: {
            Text(coordinator.externalPairErrorMessage ?? "")
        }
    }

    private var windowTitle: String {
        if let ws = coordinator.summary.workspaceName, !ws.isEmpty {
            return "Steve · \(ws)"
        }
        return "Steve"
    }

    private var statusColor: Color {
        switch coordinator.summary.phase {
        case .connected: return .green
        case .connecting: return .orange
        case .authRequired: return .yellow
        case .offline: return .red
        }
    }

    private var offlineBanner: some View {
        VStack {
            Spacer()
            HStack(spacing: 12) {
                Image(systemName: "bolt.slash.fill")
                    .foregroundStyle(.orange)
                VStack(alignment: .leading, spacing: 2) {
                    Text(L10n.tr("无法连接到 \(hubStore.activeProfile.name)", "Cannot reach \(hubStore.activeProfile.name)"))
                        .font(.callout.weight(.medium))
                    Text(coordinator.summary.errorMessage ?? hubStore.activeProfile.primaryDisplayAddress)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer()
                Button(L10n.tr("切换 Hub", "Switch Hub")) {
                    coordinator.isHubSwitcherPresented = true
                }
                .controlSize(.small)
                Button(L10n.tr("重试", "Retry")) {
                    coordinator.reconnectActiveHub()
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
            .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .strokeBorder(Color.primary.opacity(0.08), lineWidth: 1)
            )
            .frame(maxWidth: 580)
            .padding(.bottom, 18)
        }
    }
}
