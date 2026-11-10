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
            } else {
                ProgressView("正在启动 Steve 客户端…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }

            if coordinator.summary.phase == .offline {
                offlineBanner
            }
        }
        .navigationTitle(windowTitle)
        .toolbar {
            ToolbarItemGroup(placement: .primaryAction) {
                Menu {
                    Section("快速切换 Hub") {
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
                        Label("管理与添加 Hub…", systemImage: "server.rack")
                    }
                    Button {
                        coordinator.reconnectActiveHub()
                    } label: {
                        Label("重新连接", systemImage: "arrow.clockwise")
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
                .help("当前连接的 Hub（点击切换或添加其它 Hub）")

                Button {
                    coordinator.isPairQRPresented = true
                } label: {
                    Label("手机配对", systemImage: "qrcode")
                }
                .help("生成配对二维码供 iOS 扫码连接")
            }
        }
        .sheet(isPresented: $coordinator.isHubSwitcherPresented) {
            HubSwitcherSheet(coordinator: coordinator, allowsQRScan: false)
        }
        .sheet(isPresented: $coordinator.isPairQRPresented) {
            PairQRSheet(coordinator: coordinator)
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
                    Text("无法连接到 \(hubStore.activeProfile.name)")
                        .font(.callout.weight(.medium))
                    Text(coordinator.summary.errorMessage ?? hubStore.activeProfile.primaryDisplayAddress)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer()
                Button("切换 Hub") {
                    coordinator.isHubSwitcherPresented = true
                }
                .controlSize(.small)
                Button("重试") {
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
