import AppKit
import SteveClientKit
import SwiftUI

@main
struct SteveMacApp: App {
    @StateObject private var coordinator: SteveClientCoordinator

    init() {
        let hostName = Host.current().localizedName ?? "Mac"
        let version = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "1.0"
        let webRoot = Bundle.main.resourceURL?.appendingPathComponent("WebDist", isDirectory: true)
        let descriptor = ClientDescriptor(
            kind: "mac",
            name: hostName,
            platform: "macOS",
            version: version
        )
        _coordinator = StateObject(wrappedValue: SteveClientCoordinator(webRoot: webRoot, clientDescriptor: descriptor))
    }

    var body: some Scene {
        WindowGroup("Steve", id: "main") {
            MacMainWindowView(coordinator: coordinator)
                .onAppear {
                    coordinator.startIfNeeded()
                }
                .onOpenURL { url in
                    if url.scheme?.lowercased() == "steve" {
                        Task {
                            _ = try? await coordinator.pairWithQRCode(uri: url.absoluteString)
                        }
                    }
                }
        }
        .defaultSize(width: 1240, height: 820)
        .commands {
            CommandMenu("Hub") {
                Button("切换 Hub…") {
                    coordinator.isHubSwitcherPresented = true
                }
                .keyboardShortcut("H", modifiers: [.command, .shift])

                Button("手机扫码配对…") {
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

                Button("重新连接当前 Hub") {
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
            return "Steve Hub 已连接 · \(summary.route.displayTitle)"
        case .connecting:
            return "正在连接 Steve Hub…"
        case .authRequired:
            return "Steve Hub 需要登录"
        case .offline:
            return "Steve Hub 离线"
        }
    }
}
