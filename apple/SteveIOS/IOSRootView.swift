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
            if coordinator.summary.phase == .offline {
                offlinePairingLanding
            } else if coordinator.localGatewayURL != nil {
                SteveWebView(coordinator: coordinator, platformName: "ios")
                    .ignoresSafeArea(.container, edges: .bottom)
            } else {
                ProgressView("正在连接 Steve Hub…")
            }

            if coordinator.summary.phase != .offline {
                floatingHubPill
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
    }

    private var floatingHubPill: some View {
        Menu {
            Section("切换 Hub") {
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
                Label("扫码配对新 Hub…", systemImage: "qrcode.viewfinder")
            }
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

    private var offlinePairingLanding: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: 24) {
                    Spacer(minLength: 20)

                    VStack(spacing: 10) {
                        ZStack {
                            Circle()
                                .fill(Color.accentColor.opacity(0.12))
                                .frame(width: 76, height: 76)
                            Image(systemName: "qrcode.viewfinder")
                                .font(.system(size: 34, weight: .medium))
                                .foregroundStyle(Color.accentColor)
                        }
                        Text("连接到你的 Steve Hub")
                            .font(.title2.weight(.bold))
                        Text(hubStore.activeProfile.isLocalDefault
                             ? "当前未在本机发现运行中的 Hub（127.0.0.1:8440）。打开 Mac 或网页端「设置 · 访问与客户端」，扫描配对二维码即可连接（有公网默认走公网，无公网走内网）。"
                             : "无法连接到「\(hubStore.activeProfile.name)」（\(hubStore.activeProfile.primaryDisplayAddress)）。")
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                            .padding(.horizontal, 8)
                    }

                    VStack(spacing: 12) {
                        Button {
                            coordinator.isQRScannerPresented = true
                        } label: {
                            Label("扫码配对连接", systemImage: "qrcode.viewfinder")
                                .font(.headline)
                                .frame(maxWidth: .infinity)
                                .padding(.vertical, 6)
                        }
                        .buttonStyle(.borderedProminent)
                        .controlSize(.large)

                        Button {
                            coordinator.reconnectActiveHub()
                        } label: {
                            Label("重试当前 Hub（\(hubStore.activeProfile.name)）", systemImage: "arrow.clockwise")
                                .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.bordered)
                    }

                    if hubStore.profiles.count > 1 {
                        VStack(alignment: .leading, spacing: 8) {
                            Text("已保存的 Hub")
                                .font(.footnote.weight(.semibold))
                                .foregroundStyle(.secondary)
                            ForEach(hubStore.profiles) { profile in
                                Button {
                                    coordinator.switchToHub(id: profile.id)
                                } label: {
                                    HStack {
                                        VStack(alignment: .leading, spacing: 2) {
                                            Text(profile.name)
                                                .font(.body.weight(.medium))
                                                .foregroundStyle(.primary)
                                            Text(profile.primaryDisplayAddress)
                                                .font(.caption.monospaced())
                                                .foregroundStyle(.secondary)
                                        }
                                        Spacer()
                                        if profile.id == hubStore.activeHubID {
                                            Image(systemName: "checkmark.circle.fill")
                                                .foregroundStyle(Color.accentColor)
                                        } else {
                                            Image(systemName: "chevron.right")
                                                .font(.caption)
                                                .foregroundStyle(.tertiary)
                                        }
                                    }
                                    .padding(12)
                                    .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 12))
                                }
                                .buttonStyle(.plain)
                            }
                        }
                    }

                    VStack(alignment: .leading, spacing: 10) {
                        Text("或手动输入 Hub 地址")
                            .font(.footnote.weight(.semibold))
                            .foregroundStyle(.secondary)
                        HStack(spacing: 8) {
                            TextField("http://192.168.1.10:8440 或 https://…", text: $manualAddress)
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
                                    Text("连接")
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
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        coordinator.isHubSwitcherPresented = true
                    } label: {
                        Image(systemName: "server.rack")
                    }
                }
            }
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
