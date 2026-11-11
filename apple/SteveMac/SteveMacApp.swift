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

        #if DEBUG
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
        #else
        let defaults = UserDefaults.standard
        let credStore: CredentialStoring = KeychainCredentialStore()
        #endif

        let hubStore = HubStore(
            defaults: defaults,
            credentials: credStore,
            defaultProfile: LocalHubDiscovery.defaultProfile(environment: env),
            includeLocalDefault: true
        )
        let coord = SteveClientCoordinator(
            hubStore: hubStore,
            webRoot: webRoot,
            preferredPortDefaults: defaults,
            clientDescriptor: descriptor
        )
        _coordinator = StateObject(wrappedValue: coord)
        #if DEBUG
        if let portStr = env["STEVE_CLIENT_DEBUG_PORT"], let port = UInt16(portStr), port > 0 {
            MacDebugHarness.start(port: port, coordinator: coord)
        }
        #endif
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
            HubCommands(coordinator: coordinator, hubStore: coordinator.hubStore)
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

private struct HubCommands: Commands {
    @ObservedObject var coordinator: SteveClientCoordinator
    @ObservedObject var hubStore: HubStore
    @Environment(\.openWindow) private var openWindow

    var body: some Commands {
        CommandMenu("Hub") {
            Button(L10n.tr("添加 Hub…", "Add Hub…")) {
                openWindow(id: "main")
                NSApplication.shared.activate(ignoringOtherApps: true)
                coordinator.navigate(hashRoute: "#/hub/add")
            }
            .keyboardShortcut("H", modifiers: [.command, .shift])

            Button(L10n.tr("配对新设备…", "Pair Device…")) {
                openWindow(id: "main")
                NSApplication.shared.activate(ignoringOtherApps: true)
                coordinator.navigate(hashRoute: "#/hub/pair")
            }
            .keyboardShortcut("P", modifiers: [.command, .shift])

            Divider()

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

            Divider()

            Button(L10n.tr("重新连接当前 Hub", "Reconnect Current Hub")) {
                coordinator.reconnectActiveHub()
            }
            .keyboardShortcut("R", modifiers: [.command, .shift])
        }
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

#if DEBUG
import Network
import WebKit

private enum MacDebugHarness {
    private static var listener: NWListener?
    private static var previewWindow: NSWindow?

    static func start(port: UInt16, coordinator: SteveClientCoordinator) {
        guard let nwPort = NWEndpoint.Port(rawValue: port) else { return }
        let params = NWParameters.tcp
        params.requiredLocalEndpoint = NWEndpoint.hostPort(host: .ipv4(.loopback), port: nwPort)
        guard let l = try? NWListener(using: params) else { return }
        l.newConnectionHandler = { conn in
            conn.start(queue: .global())
            conn.receive(minimumIncompleteLength: 1, maximumLength: 131_072) { data, _, _, _ in
                let raw = String(data: data ?? Data(), encoding: .utf8) ?? ""
                guard let split = raw.range(of: "\r\n\r\n") else {
                    conn.cancel()
                    return
                }
                let bodyData = Data(raw[split.upperBound...].utf8)
                let req = (try? JSONSerialization.jsonObject(with: bodyData) as? [String: Any]) ?? [:]
                Task { @MainActor in
                    let resp = await handle(req, coordinator: coordinator)
                    let payload = (try? JSONSerialization.data(withJSONObject: resp)) ?? Data("{}".utf8)
                    let head = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: \(payload.count)\r\nConnection: close\r\n\r\n"
                    conn.send(content: Data(head.utf8) + payload, completion: .contentProcessed { _ in conn.cancel() })
                }
            }
        }
        l.start(queue: DispatchQueue(label: "app.steve.mac.debug"))
        listener = l
    }

    @MainActor
    private static func handle(_ req: [String: Any], coordinator: SteveClientCoordinator) async -> [String: Any] {
        let cmd = req["cmd"] as? String ?? "status"
        switch cmd {
        case "status":
            let winIDs = NSApplication.shared.windows.map { [
                "number": $0.windowNumber,
                "title": $0.title,
                "visible": $0.isVisible,
                "class": String(describing: type(of: $0)),
            ] as [String: Any] }
            return [
                "ok": true,
                "gateway": coordinator.localGatewayURL?.absoluteString as Any,
                "activeHubID": coordinator.hubStore.activeHubID,
                "phase": coordinator.summary.phase.rawValue,
                "pendingPair": coordinator.pendingExternalPairConfirmation?.displayName as Any,
                "pendingEndpoints": coordinator.pendingExternalPairConfirmation?.endpointsSummary as Any,
                "pairError": coordinator.externalPairErrorMessage as Any,
                "windows": winIDs,
            ]
        case "eval":
            guard let wv = SteveWebView.BridgeCoordinator.debugLastAttachedWebView,
                  let script = req["script"] as? String else {
                return ["ok": false, "error": "no webview or script"]
            }
            let args = (req["args"] as? [String: Any]) ?? [:]
            do {
                let val = try await wv.callAsyncJavaScript(script, arguments: args, in: nil, contentWorld: .page)
                return ["ok": true, "result": val ?? NSNull()]
            } catch {
                return ["ok": false, "error": error.localizedDescription]
            }
        case "snapshot":
            guard let path = req["path"] as? String else { return ["ok": false, "error": "missing path"] }
            let target = (req["target"] as? String) ?? "webview"
            if target == "menubar" {
                let host = NSHostingView(rootView: MacMenuBarPopoverView(coordinator: coordinator))
                let size = host.fittingSize
                host.frame = NSRect(origin: .zero, size: NSSize(width: max(size.width, 320), height: max(size.height, 360)))
                let win = NSWindow(
                    contentRect: host.frame,
                    styleMask: [.titled, .fullSizeContentView],
                    backing: .buffered,
                    defer: false
                )
                win.title = "Steve MenuBar"
                win.contentView = host
                win.orderFrontRegardless()
                previewWindow = win
                try? await Task.sleep(nanoseconds: 150_000_000)
                guard let rep = host.bitmapImageRepForCachingDisplay(in: host.bounds) else {
                    return ["ok": false, "error": "bitmap rep failed"]
                }
                host.cacheDisplay(in: host.bounds, to: rep)
                guard let png = rep.representation(using: .png, properties: [:]) else {
                    return ["ok": false, "error": "png failed"]
                }
                try? png.write(to: URL(fileURLWithPath: path))
                let num = win.windowNumber
                return ["ok": true, "windowNumber": num]
            }
            guard let wv = SteveWebView.BridgeCoordinator.debugLastAttachedWebView else {
                return ["ok": false, "error": "no webview"]
            }
            do {
                let img = try await wv.takeSnapshot(configuration: nil)
                guard let tiff = img.tiffRepresentation,
                      let rep = NSBitmapImageRep(data: tiff),
                      let png = rep.representation(using: .png, properties: [:]) else {
                    return ["ok": false, "error": "png encode failed"]
                }
                try png.write(to: URL(fileURLWithPath: path))
                return ["ok": true, "windowNumber": wv.window?.windowNumber ?? 0]
            } catch {
                return ["ok": false, "error": error.localizedDescription]
            }
        case "setOpenPanel":
            if let paths = req["paths"] as? [String] {
                SteveWebView.BridgeCoordinator.debugNextOpenPanelURLs = paths.map { URL(fileURLWithPath: $0) }
            }
            return ["ok": true]
        case "clickAt":
            guard let wv = SteveWebView.BridgeCoordinator.debugLastAttachedWebView,
                  let win = wv.window,
                  let x = (req["x"] as? NSNumber)?.doubleValue,
                  let y = (req["y"] as? NSNumber)?.doubleValue else {
                return ["ok": false, "error": "missing webview, window, or coords"]
            }
            // Convert CSS top-left coordinates inside `wv` into AppKit bottom-left window coordinates.
            let localPt = NSPoint(x: x, y: wv.isFlipped ? y : (wv.bounds.height - y))
            let winPt = wv.convert(localPt, to: nil)
            let now = ProcessInfo.processInfo.systemUptime
            if let down = NSEvent.mouseEvent(
                with: .leftMouseDown, location: winPt, modifierFlags: [],
                timestamp: now, windowNumber: win.windowNumber, context: nil,
                eventNumber: 1, clickCount: 1, pressure: 1.0
            ), let up = NSEvent.mouseEvent(
                with: .leftMouseUp, location: winPt, modifierFlags: [],
                timestamp: now + 0.02, windowNumber: win.windowNumber, context: nil,
                eventNumber: 2, clickCount: 1, pressure: 0.0
            ) {
                win.sendEvent(down)
                win.sendEvent(up)
                return ["ok": true]
            }
            return ["ok": false, "error": "mouseEvent failed"]
        case "confirmPair":
            let profile = await coordinator.confirmPendingExternalPairing()
            return ["ok": profile != nil, "id": profile?.id as Any]
        case "cancelPair":
            coordinator.cancelPendingExternalPairing()
            return ["ok": true]
        case "openURI":
            if let uri = req["uri"] as? String {
                coordinator.requestExternalPairingConfirmation(uri: uri)
            }
            return ["ok": true]
        case "navigate":
            if let route = req["route"] as? String {
                coordinator.navigate(hashRoute: route)
            }
            return ["ok": true]
        case "switchHub":
            if let id = req["id"] as? String {
                coordinator.switchToHub(id: id)
            }
            return ["ok": true]
        case "quit":
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.1) {
                NSApplication.shared.terminate(nil)
            }
            return ["ok": true]
        default:
            return ["ok": false, "error": "unknown cmd"]
        }
    }
}
#endif
