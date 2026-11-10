import SteveClientKit
import SwiftUI

struct IOSRootView: View {
    @ObservedObject var coordinator: SteveClientCoordinator
    @ObservedObject private var hubStore: HubStore
    @State private var manualAddress = ""
    @State private var isConnectingManual = false
    @State private var manualError: String?

    init(coordinator: SteveClientCoordinator) {
        self.coordinator = coordinator
        self.hubStore = coordinator.hubStore
    }

    var body: some View {
        ZStack(alignment: .topTrailing) {
            if !hubStore.hasProfiles {
                onboardingPairingLanding
            } else if coordinator.localGatewayURL != nil {
                SteveWebView(coordinator: coordinator, platformName: "ios")
                    .ignoresSafeArea(.container, edges: .bottom)

                floatingHubPill

                if coordinator.summary.phase == .offline {
                    offlineBanner
                }
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
                        .padding(.horizontal, 24)
                    Button(L10n.tr("重试", "Retry")) {
                        coordinator.retryGatewayStartup()
                    }
                    .buttonStyle(.borderedProminent)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ProgressView(L10n.tr("正在连接 Steve Hub…", "Connecting to Steve Hub…"))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .sheet(isPresented: $coordinator.isHubSwitcherPresented) {
            HubSwitcherSheet(coordinator: coordinator, allowsQRScan: true)
        }
        .sheet(isPresented: $coordinator.isPairQRPresented) {
            NavigationStack {
                PairQRSheet(coordinator: coordinator)
            }
        }
        .fullScreenCover(isPresented: $coordinator.isQRScannerPresented) {
            QRCodeScannerSheet(coordinator: coordinator)
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

    private var floatingHubPill: some View {
        Menu {
            Section(L10n.tr("切换 Hub", "Switch Hub")) {
                ForEach(hubStore.profiles) { profile in
                    Button {
                        coordinator.switchToHub(id: profile.id)
                    } label: {
                        if profile.id == hubStore.activeHubID {
                            Label(profile.name, systemImage: "checkmark")
                        } else {
                            Text(profile.name)
                        }
                    }
                }
            }
            Divider()
            Button {
                coordinator.isQRScannerPresented = true
            } label: {
                Label(L10n.tr("扫码配对新 Hub…", "Scan QR to Pair…"), systemImage: "qrcode.viewfinder")
            }
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
            HStack(spacing: 5) {
                Circle()
                    .fill(statusColor)
                    .frame(width: 7, height: 7)
                Text(hubStore.activeProfile.name)
                    .font(.caption.weight(.medium))
                    .lineLimit(1)
                Image(systemName: "chevron.down")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(.secondary)
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(.ultraThinMaterial, in: Capsule())
            .overlay(Capsule().strokeBorder(Color.primary.opacity(0.08), lineWidth: 0.5))
        }
        .padding(.top, 8)
        .padding(.trailing, 12)
    }

    private var offlineBanner: some View {
        VStack {
            Spacer()
            HStack(spacing: 10) {
                Image(systemName: "wifi.slash")
                    .foregroundStyle(.orange)
                VStack(alignment: .leading, spacing: 2) {
                    Text(L10n.tr("无法连接 \(hubStore.activeProfile.name)", "Cannot reach \(hubStore.activeProfile.name)"))
                        .font(.footnote.weight(.semibold))
                        .lineLimit(1)
                    Text(coordinator.summary.errorMessage ?? hubStore.activeProfile.primaryDisplayAddress)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 4)
                Button {
                    coordinator.isQRScannerPresented = true
                } label: {
                    Image(systemName: "qrcode.viewfinder")
                }
                .buttonStyle(.bordered)
                .controlSize(.small)

                Button(L10n.tr("切换", "Switch")) {
                    coordinator.isHubSwitcherPresented = true
                }
                .buttonStyle(.bordered)
                .controlSize(.small)

                Button(L10n.tr("重试", "Retry")) {
                    coordinator.reconnectActiveHub()
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.small)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .background(.ultraThinMaterial, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .strokeBorder(Color.primary.opacity(0.08), lineWidth: 1)
            )
            .padding(.horizontal, 12)
            .padding(.bottom, 12)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var onboardingPairingLanding: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 24) {
                    Spacer(minLength: 24)

                    VStack(spacing: 10) {
                        ZStack {
                            Circle()
                                .fill(Color.accentColor.opacity(0.12))
                                .frame(width: 76, height: 76)
                            Image(systemName: "qrcode.viewfinder")
                                .font(.system(size: 34, weight: .medium))
                                .foregroundStyle(Color.accentColor)
                        }
                        Text(L10n.tr("连接到你的 Steve Hub", "Connect to Your Steve Hub"))
                            .font(.title2.weight(.bold))
                        Text(
                            L10n.tr(
                                "打开 Mac 客户端或网页端「设置 · 访问与客户端」，扫描配对二维码即可一键登录并连接（有公网地址默认走公网连接，无公网走局域网）。",
                                "Open Mac or Web Settings -> Access & Clients and scan the pairing QR code to sign in and connect (uses public URL by default when configured, otherwise LAN)."
                            )
                        )
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, 8)
                    }

                    Button {
                        coordinator.isQRScannerPresented = true
                    } label: {
                        Label(L10n.tr("扫码配对连接", "Scan Pairing QR Code"), systemImage: "qrcode.viewfinder")
                            .font(.headline)
                            .frame(maxWidth: .infinity)
                            .padding(.vertical, 6)
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)

                    VStack(alignment: .leading, spacing: 10) {
                        Text(L10n.tr("或手动输入 Hub 地址", "Or Enter Hub URL Manually"))
                            .font(.footnote.weight(.semibold))
                            .foregroundStyle(.secondary)
                        HStack(spacing: 8) {
                            TextField("http://192.168.1.10:8440 / https://…", text: $manualAddress)
                                .textFieldStyle(.roundedBorder)
                                .keyboardType(.URL)
                                .textInputAutocapitalization(.never)
                                .autocorrectionDisabled()
                            Button {
                                Task { await connectManual() }
                            } label: {
                                if isConnectingManual {
                                    ProgressView()
                                } else {
                                    Text(L10n.tr("连接", "Connect"))
                                }
                            }
                            .buttonStyle(.bordered)
                            .disabled(isConnectingManual || manualAddress.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                        }
                        if let manualError {
                            Text(manualError)
                                .font(.caption)
                                .foregroundStyle(.red)
                        }
                    }
                }
                .padding(24)
            }
            .navigationTitle("Steve")
            .navigationBarTitleDisplayMode(.inline)
        }
    }

    private func connectManual() async {
        isConnectingManual = true
        manualError = nil
        do {
            try await coordinator.addAndSwitchManualHub(address: manualAddress, name: nil)
            manualAddress = ""
        } catch {
            manualError = error.localizedDescription
        }
        isConnectingManual = false
    }

    private var statusColor: Color {
        switch coordinator.summary.phase {
        case .connected: return .green
        case .connecting: return .orange
        case .authRequired: return .yellow
        case .offline: return .red
        }
    }
}
