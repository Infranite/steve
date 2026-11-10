import SwiftUI

public struct HubSwitcherSheet: View {
    @ObservedObject private var coordinator: SteveClientCoordinator
    @ObservedObject private var hubStore: HubStore
    private let allowsQRScan: Bool

    @Environment(\.dismiss) private var dismiss
    @State private var newAddress = ""
    @State private var newName = ""
    @State private var isBusy = false
    @State private var errorMessage: String?

    public init(coordinator: SteveClientCoordinator, allowsQRScan: Bool) {
        self.coordinator = coordinator
        self.hubStore = coordinator.hubStore
        self.allowsQRScan = allowsQRScan
    }

    public var body: some View {
        NavigationStack {
            Form {
                Section(L10n.tr("已配置的 Hub", "Configured Hubs")) {
                    if hubStore.profiles.isEmpty {
                        Text(L10n.tr("尚未添加任何 Hub，请通过下方扫码或手动输入地址接入。", "No Hubs configured yet. Scan a pairing QR code or add an address below."))
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                    ForEach(hubStore.profiles) { profile in
                        HStack(spacing: 12) {
                            Button {
                                coordinator.switchToHub(id: profile.id)
                                dismiss()
                            } label: {
                                HStack(spacing: 10) {
                                    Image(systemName: profile.id == hubStore.activeHubID ? "checkmark.circle.fill" : "circle")
                                        .foregroundStyle(profile.id == hubStore.activeHubID ? Color.accentColor : .secondary)
                                    VStack(alignment: .leading, spacing: 2) {
                                        HStack(spacing: 6) {
                                            Text(profile.name)
                                                .font(.body.weight(.medium))
                                                .foregroundStyle(.primary)
                                            if profile.isLocalDefault {
                                                Text(L10n.tr("默认", "Default"))
                                                    .font(.caption2)
                                                    .padding(.horizontal, 6)
                                                    .padding(.vertical, 1)
                                                    .background(Color.secondary.opacity(0.15), in: Capsule())
                                                    .foregroundStyle(.secondary)
                                            }
                                            if profile.publicURL != nil {
                                                Text(L10n.tr("公网优先", "Public first"))
                                                    .font(.caption2)
                                                    .padding(.horizontal, 6)
                                                    .padding(.vertical, 1)
                                                    .background(Color.blue.opacity(0.14), in: Capsule())
                                                    .foregroundStyle(.blue)
                                            }
                                        }
                                        Text(profile.primaryDisplayAddress)
                                            .font(.caption.monospaced())
                                            .foregroundStyle(.secondary)
                                            .lineLimit(1)
                                    }
                                    Spacer()
                                    if let ver = profile.lastKnownVersion {
                                        Text(ver)
                                            .font(.caption2.monospaced())
                                            .foregroundStyle(.secondary)
                                    }
                                }
                            }
                            .buttonStyle(.plain)

                            if !profile.isLocalDefault {
                                Button(role: .destructive) {
                                    coordinator.removeHub(id: profile.id)
                                } label: {
                                    Image(systemName: "trash")
                                        .font(.caption)
                                }
                                .buttonStyle(.borderless)
                                .help(L10n.tr("移除此 Hub", "Remove this Hub"))
                            }
                        }
                    }
                }

                if allowsQRScan {
                    Section(L10n.tr("扫码快速接入", "Pair by QR Code")) {
                        Button {
                            dismiss()
                            DispatchQueue.main.asyncAfter(deadline: .now() + 0.25) {
                                coordinator.isQRScannerPresented = true
                            }
                        } label: {
                            Label(
                                L10n.tr("扫描 Mac / Web 上的配对二维码", "Scan pairing QR code from Mac or Web"),
                                systemImage: "qrcode.viewfinder"
                            )
                        }
                    }
                }

                Section(L10n.tr("手动接入其它 Hub", "Connect Another Hub")) {
                    TextField(
                        L10n.tr(
                            "Hub 地址（如 https://hub.example.com 或 192.168.1.10:8440）",
                            "Hub URL (e.g. https://hub.example.com or 192.168.1.10:8440)"
                        ),
                        text: $newAddress
                    )
                    .autocorrectionDisabled()
                    #if os(iOS)
                    .textInputAutocapitalization(.never)
                    .keyboardType(.URL)
                    #endif
                    TextField(L10n.tr("备注名称（可选）", "Display name (optional)"), text: $newName)

                    if let errorMessage {
                        Text(errorMessage)
                            .font(.footnote)
                            .foregroundStyle(.red)
                    }

                    Button {
                        Task { await addHub() }
                    } label: {
                        HStack {
                            if isBusy { ProgressView().controlSize(.small) }
                            Text(L10n.tr("连接并切换", "Connect & Switch"))
                        }
                    }
                    .disabled(isBusy || newAddress.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
            .formStyle(.grouped)
            .navigationTitle(L10n.tr("切换 Hub", "Switch Hub"))
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(L10n.tr("关闭", "Close")) { dismiss() }
                }
            }
        }
        #if os(macOS)
        .frame(minWidth: 460, minHeight: 380)
        #endif
    }

    private func addHub() async {
        isBusy = true
        errorMessage = nil
        do {
            try await coordinator.addAndSwitchManualHub(address: newAddress, name: newName)
            newAddress = ""
            newName = ""
            dismiss()
        } catch {
            errorMessage = error.localizedDescription
        }
        isBusy = false
    }
}

public struct PairQRSheet: View {
    @ObservedObject private var coordinator: SteveClientCoordinator
    @Environment(\.dismiss) private var dismiss

    @State private var pairing: CreatedPairing?
    @State private var claimStatus: PairingClaimStatus?
    @State private var isLoading = false
    @State private var errorMessage: String?

    public init(coordinator: SteveClientCoordinator) {
        self.coordinator = coordinator
    }

    public var body: some View {
        VStack(spacing: 18) {
            VStack(spacing: 6) {
                Text(L10n.tr("手机扫码配对", "Pair Mobile App"))
                    .font(.title3.weight(.semibold))
                Text(
                    L10n.tr(
                        "打开 Steve iOS App 扫描二维码即可配对当前 Hub。有公网地址默认走公网连接，无公网地址走局域网 / 内网。",
                        "Scan with the Steve iOS app to pair with this Hub. Connects via public URL when configured, otherwise over LAN."
                    )
                )
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            }

            if isLoading && pairing == nil {
                ProgressView()
                    .padding(.vertical, 40)
            } else if let errorMessage {
                VStack(spacing: 12) {
                    Image(systemName: "exclamationmark.triangle")
                        .font(.largeTitle)
                        .foregroundStyle(.orange)
                    Text(errorMessage)
                        .font(.callout)
                        .multilineTextAlignment(.center)
                        .foregroundStyle(.secondary)
                    HStack(spacing: 10) {
                        Button(L10n.tr("在主窗口中登录", "Sign in in Main Window")) {
                            coordinator.navigate(hashRoute: "#/settings/access")
                            dismiss()
                        }
                        Button(L10n.tr("重试", "Retry")) {
                            Task { await loadPairing() }
                        }
                        .buttonStyle(.borderedProminent)
                    }
                }
                .padding(.vertical, 24)
            } else if let pairing {
                if pairing.loopback_only {
                    VStack(spacing: 10) {
                        Image(systemName: "network.slash")
                            .font(.largeTitle)
                            .foregroundStyle(.orange)
                        Text(
                            L10n.tr(
                                "当前 Hub 仅监听 127.0.0.1 回环地址，手机无法直接连接。请配置 STEVE_PUBLIC_URL 或绑定局域网地址（如 STEVE_HOST=0.0.0.0）后刷新。",
                                "This Hub only listens on 127.0.0.1 loopback, so a phone cannot reach it. Set STEVE_PUBLIC_URL or bind a LAN interface (e.g. STEVE_HOST=0.0.0.0), then refresh."
                            )
                        )
                        .font(.footnote)
                        .foregroundStyle(.orange)
                        .multilineTextAlignment(.center)
                    }
                    .padding(18)
                    .frame(maxWidth: .infinity)
                    .background(Color.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
                } else {
                    if let image = NativeQRGenerator.makeImage(from: pairing.uri, scale: 9) {
                        Group {
                            #if canImport(AppKit)
                            Image(nsImage: image)
                                .interpolation(.none)
                                .resizable()
                            #elseif canImport(UIKit)
                            Image(uiImage: image)
                                .interpolation(.none)
                                .resizable()
                            #endif
                        }
                        .frame(width: 216, height: 216)
                        .padding(14)
                        .background(Color.white, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
                        .overlay(
                            RoundedRectangle(cornerRadius: 16, style: .continuous)
                                .strokeBorder(Color.primary.opacity(0.1), lineWidth: 1)
                        )
                    }

                    if pairing.public_url != nil {
                        Label(
                            L10n.tr("默认走公网连接（内网备用）", "Public route by default (LAN fallback)"),
                            systemImage: "globe"
                        )
                        .font(.footnote.weight(.medium))
                        .padding(.horizontal, 10)
                        .padding(.vertical, 4)
                        .background(Color.blue.opacity(0.12), in: Capsule())
                        .foregroundStyle(.blue)
                    } else {
                        Label(
                            L10n.tr("走内网 / 局域网连接（未配置公网地址）", "LAN route (no public URL configured)"),
                            systemImage: "wifi"
                        )
                        .font(.footnote.weight(.medium))
                        .padding(.horizontal, 10)
                        .padding(.vertical, 4)
                        .background(Color.green.opacity(0.12), in: Capsule())
                        .foregroundStyle(.green)
                    }

                    VStack(alignment: .leading, spacing: 4) {
                        if let pub = pairing.public_url {
                            HStack {
                                Text(pub).font(.caption.monospaced()).lineLimit(1)
                                Spacer()
                                Text(L10n.tr("公网", "Public")).font(.caption2).foregroundStyle(.secondary)
                            }
                        }
                        ForEach(pairing.lan_urls, id: \.self) { lan in
                            HStack {
                                Text(lan).font(.caption.monospaced()).lineLimit(1)
                                Spacer()
                                Text(L10n.tr("内网", "LAN")).font(.caption2).foregroundStyle(.secondary)
                            }
                        }
                    }
                    .padding(10)
                    .frame(maxWidth: .infinity)
                    .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))

                    statusRow
                }
            }

            HStack {
                if let pairing {
                    if !pairing.loopback_only {
                        Button {
                            copyToPasteboard(pairing.uri)
                        } label: {
                            Label(L10n.tr("复制配对链接", "Copy Link"), systemImage: "doc.on.doc")
                        }
                    }
                    Button {
                        Task { await loadPairing() }
                    } label: {
                        Label(L10n.tr("刷新", "Refresh"), systemImage: "arrow.clockwise")
                    }
                }
                Spacer()
                Button(L10n.tr("完成", "Done")) { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(24)
        #if os(macOS)
        .frame(width: 420)
        #endif
        .task {
            await loadPairing()
        }
        .task(id: pairing?.id) {
            guard let pairing, !pairing.loopback_only else { return }
            let id = pairing.id
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                guard !Task.isCancelled else { break }
                if let status = try? await coordinator.pollPairingStatus(id: id) {
                    claimStatus = status
                    if (status.status == "claimed" && status.client != nil) || status.status == "expired" {
                        break
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var statusRow: some View {
        let state = claimStatus?.status ?? "pending"
        if state == "claimed", let client = claimStatus?.client {
            Label(
                L10n.tr("已配对：\(client.name)", "Paired: \(client.name)"),
                systemImage: "checkmark.circle.fill"
            )
            .font(.footnote.weight(.medium))
            .foregroundStyle(.green)
        } else if state == "expired" {
            Label(
                L10n.tr("二维码已过期，请点击刷新", "QR code expired — click Refresh"),
                systemImage: "clock.badge.exclamationmark"
            )
            .font(.footnote)
            .foregroundStyle(.red)
        } else {
            HStack(spacing: 6) {
                ProgressView().controlSize(.small)
                Text(L10n.tr("等待手机扫码确认…", "Waiting for mobile scan…"))
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func loadPairing() async {
        isLoading = true
        errorMessage = nil
        claimStatus = nil
        do {
            pairing = try await coordinator.createPairingForActiveHub()
        } catch {
            errorMessage = error.localizedDescription
        }
        isLoading = false
    }

    private func copyToPasteboard(_ text: String) {
        #if canImport(AppKit)
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(text, forType: .string)
        #elseif canImport(UIKit)
        UIPasteboard.general.string = text
        #endif
    }
}
