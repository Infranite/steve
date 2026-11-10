import AppKit
import SteveClientKit
import SwiftUI

@main
struct SteveMacApp: App {
    @StateObject private var coordinator: SteveClientCoordinator

    init() {
        let env = ProcessInfo.processInfo.environment
        let hostName = Host.current().localizedName ?? "Mac"
        let version = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "1.0"
        let webRoot = Bundle.main.resourceURL?.appendingPathComponent("WebDist", isDirectory: true)
        let descriptor = ClientDescriptor(
            kind: "mac",
            name: hostName,
            platform: "macOS",
            version: version
        )

        let defaults: UserDefaults
        if let suite = env["STEVE_CLIENT_DEFAULTS_SUITE"], !suite.isEmpty,
           let customDefaults = UserDefaults(suiteName: suite) {
            defaults = customDefaults
        } else {
            defaults = .standard
        }

        let credStore: CredentialStoring
        if env["STEVE_CLIENT_KEYCHAIN_EPHEMERAL"] == "1" {
            credStore = InMemoryCredentialStore()
        } else if let service = env["STEVE_CLIENT_KEYCHAIN_SERVICE"], !service.isEmpty {
            credStore = KeychainCredentialStore(service: service)
        } else {
            credStore = KeychainCredentialStore()
        }

        let hubStore = HubStore(
            defaults: defaults,
            credentials: credStore,
            defaultProfile: LocalHubDiscovery.defaultProfile(environment: env),
            includeLocalDefault: true
        )
        _coordinator = StateObject(
            wrappedValue: SteveClientCoordinator(
                hubStore: hubStore,
                webRoot: webRoot,
                preferredPortDefaults: defaults,
                clientDescriptor: descriptor
            )
        )
    }

    var body: some Scene {
        WindowGroup("Steve", id: "main") {
            MacMainWindowView(coordinator: coordinator)
                .onAppear {
                    coordinator.startIfNeeded()
                }
                .onOpenURL { url in
                    if url.scheme?.lowercased() == "steve" {
                        coordinator.requestExternalPairingConfirmation(uri: url.absoluteString)
                    }
                }
        }
        .defaultSize(width: 1240, height: 820)
        .commands {
            CommandMenu("Hub") {
                Button(L10n.tr("切换 Hub…", "Switch Hub…")) {
                    coordinator.isHubSwitcherPresented = true
                }
                .keyboardShortcut("H", modifiers: [.command, .shift])

                Button(L10n.tr("手机扫码配对…", "Pair Mobile App…")) {
                    coordinator.isPairQRPresented = true
                }
                .keyboardShortcut("P", modifiers: [.command, .shift])

                Divider()

                ForEach(coordinator.hubStore.profiles) { profile in
                    Button {
                        coordinator.switchToHub(id: profile.id)
                    } label: {
                        if profile.id == coordinator.hubStore.activeHubID {
                            Label(profile.name, systemImage: "checkmark")
                        } else {
                            Text(profile.name)
                        }
                    }
                }

                Divider()

                Button(L10n.tr("重新连接当前 Hub", "Reconnect Current Hub")) {
                    coordinator.reconnectActiveHub()
                }
                .keyboardShortcut("R", modifiers: [.command, .shift])
            }
        }

        MenuBarExtra {
            MacMenuBarPopoverView(coordinator: coordinator)
        } label: {
            MacMenuBarIconLabel(summary: coordinator.summary)
                .task {
                    coordinator.startIfNeeded()
                }
        }
        .menuBarExtraStyle(.window)
    }
}

private struct MacMenuBarIconLabel: View {
    let summary: HubRuntimeSummary

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: symbolName)
            if summary.openDecisions > 0 {
                Text("\(summary.openDecisions)")
                    .font(.caption2.monospacedDigit())
            }
        }
        .help(tooltipText)
    }

    private var symbolName: String {
        switch summary.phase {
        case .connected:
            if summary.openDecisions > 0 {
                return "sparkles.rectangle.stack.fill"
            }
            return summary.steveThinking || summary.runningSessions > 0 ? "bolt.horizontal.circle.fill" : "circle.hexagongrid.fill"
        case .connecting:
            return "arrow.triangle.2.circlepath.circle"
        case .authRequired:
            return "lock.circle"
        case .offline:
            return "exclamationmark.triangle"
        }
    }

    private var tooltipText: String {
        switch summary.phase {
        case .connected:
            return L10n.tr(
                "Steve Hub 已连接 · \(summary.route.displayTitle)",
                "Steve Hub connected · \(summary.route.displayTitle)"
            )
        case .connecting:
            return L10n.tr("正在连接 Steve Hub…", "Connecting to Steve Hub…")
        case .authRequired:
            return L10n.tr("Steve Hub 需要登录", "Steve Hub requires sign-in")
        case .offline:
            return L10n.tr("Steve Hub 离线", "Steve Hub offline")
        }
    }
}
