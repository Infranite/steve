import AppKit
import SteveClientKit
import SwiftUI

@main
struct SteveMacApp: App {
    @NSApplicationDelegateAdaptor(MacApplicationDelegate.self) private var appDelegate
    @StateObject private var coordinator: SteveClientCoordinator
    @StateObject private var attention: MacAttentionController

    init() {
        let env = ProcessInfo.processInfo.environment
        #if DEBUG
        let hostName = env["STEVE_CLIENT_DEVICE_NAME"].flatMap { $0.isEmpty ? nil : $0 }
            ?? Host.current().localizedName
            ?? "Mac"
        #else
        let hostName = Host.current().localizedName ?? "Mac"
        #endif
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
            clientDescriptor: descriptor,
            nativeShellChromeEnabled: true,
            nativeShellLanguageEnabled: true
        )
        coord.pairConfirmationVisibilityProbe = { MacPairConfirmation.isAlertOnScreen() }
        _coordinator = StateObject(wrappedValue: coord)
        let attentionController = MacAttentionController(coordinator: coord, defaults: defaults)
        _attention = StateObject(wrappedValue: attentionController)
        #if DEBUG
        if let portStr = env["STEVE_CLIENT_DEBUG_PORT"],
           let port = UInt16(portStr),
           port > 0,
           let token = env["STEVE_CLIENT_DEBUG_TOKEN"],
           token.utf8.count >= 16 {
            MacDebugHarness.start(
                port: port,
                expectedToken: token,
                snapshotDir: env["STEVE_CLIENT_DEBUG_DIR"],
                coordinator: coord,
                attention: attentionController
            )
        }
        #endif
    }

    var body: some Scene {
        Window("Steve", id: "main") {
            MacMainWindowView(coordinator: coordinator, attention: attention)
                .onAppear {
                    coordinator.startIfNeeded()
                    appDelegate.coordinator = coordinator; appDelegate.attention = attention
                }
        }
        .windowToolbarStyle(.unifiedCompact(showsTitle: false))
        .defaultSize(width: 1240, height: 820)
        .commands {
            HubCommands(coordinator: coordinator, hubStore: coordinator.hubStore)
            MacNavigationCommands(coordinator: coordinator)
        }

        Settings { MacClientSettingsView(attention: attention) }

        MenuBarExtra {
            MacMenuBarPopoverView(coordinator: coordinator, attention: attention)
        } label: {
            MacMenuBarIconLabel(summary: coordinator.summary, attention: attention)
                .task {
                    coordinator.startIfNeeded()
                }
                .modifier(MacMainWindowLifecycle(coordinator: coordinator))
                .onAppear { appDelegate.coordinator = coordinator; appDelegate.attention = attention }
        }
        .menuBarExtraStyle(.window)
    }
}

/// The pairing confirmation is an alert on the main window.
enum MacPairConfirmation {
    /// Is the confirmation alert on screen: a visible sheet that carries the confirmation's title.
    @MainActor
    static func isAlertOnScreen() -> Bool {
        let title = SteveClientCoordinator.pairConfirmationTitle
        return NSApplication.shared.windows.contains { window in
            guard window.isVisible, window.isSheet || window.sheetParent != nil,
                  let root = window.contentView else { return false }
            return containsLabel(title, in: root)
        }
    }

    private static func containsLabel(_ text: String, in view: NSView) -> Bool {
        if let field = view as? NSTextField, field.stringValue == text {
            return true
        }
        return view.subviews.contains { containsLabel(text, in: $0) }
    }

    @MainActor
    static var mainWindows: [NSWindow] { MacMainWindow.windows }

    @MainActor
    static func bringUpMainWindow(open: @escaping () -> Void) {
        MacMainWindow.bringUpMainWindow(open: open)
    }
}


private struct HubCommands: Commands {
    @ObservedObject private var language = NativeUILanguageState.shared
    @ObservedObject var coordinator: SteveClientCoordinator
    @ObservedObject var hubStore: HubStore
    @Environment(\.openWindow) private var openWindow

    var body: some Commands {
        CommandMenu("Hub") {
            Button(L10n.tr("添加 Hub…", "Add Hub…")) {
                MacMainWindow.navigate("#/hub/add", coordinator: coordinator, open: { openWindow(id: "main") })
            }
            .keyboardShortcut("H", modifiers: [.command, .shift])

            Button(L10n.tr("配对新设备…", "Pair Device…")) {
                MacMainWindow.navigate("#/hub/pair", coordinator: coordinator, open: { openWindow(id: "main") })
            }
            .keyboardShortcut("P", modifiers: [.command, .shift])

            Divider()

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

            Button(L10n.tr("重新连接当前 Hub", "Reconnect Current Hub")) {
                coordinator.reconnectActiveHub()
            }
            .keyboardShortcut("R", modifiers: [.command, .shift])
        }
    }
}

private struct MacMenuBarIconLabel: View {
    @ObservedObject private var language = NativeUILanguageState.shared
    let summary: HubRuntimeSummary
    @ObservedObject var attention: MacAttentionController

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: attention.state.hasAttention ? "bell.badge" : symbolName)
        }
        .help(tooltipText)
    }

    private var symbolName: String {
        switch summary.phase {
        case .connected:
            return "circle.hexagongrid.fill"
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

/// Loopback-only automation harness used for local end-to-end verification of `SteveMac` in environments
/// where macOS Accessibility / `System Events` AppleEvents are unavailable (`AXIsProcessTrusted() == false`).
///
/// Security invariants:
/// - Compiled out of `Release` builds (`#if DEBUG`).
/// - Inert unless BOTH `STEVE_CLIENT_DEBUG_PORT` and `STEVE_CLIENT_DEBUG_TOKEN` (>= 16 UTF-8 bytes) are set.
/// - Every HTTP request must supply `X-Steve-Debug-Token` matching `STEVE_CLIENT_DEBUG_TOKEN` in constant time.
/// - Never bypasses the pairing confirmation alert: alert approval is driven only via `clickAlertButton`,
///   which dispatches a real left-mouse down/up click (or `performClick`) on the live `NSAlert` sheet's `NSButton`.
/// - Snapshot writes are restricted to `.png` files inside `STEVE_CLIENT_DEBUG_DIR`.
private enum MacDebugHarness {
    private static var listener: NWListener?
    private static var previewWindow: NSWindow?

    static func start(
        port: UInt16,
        expectedToken: String,
        snapshotDir: String?,
        coordinator: SteveClientCoordinator,
        attention: MacAttentionController
    ) {
        guard let nwPort = NWEndpoint.Port(rawValue: port),
              expectedToken.utf8.count >= 16 else { return }
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
                let headerLines = raw[..<split.lowerBound].components(separatedBy: "\r\n")
                let suppliedHeaderToken = headerLines.dropFirst().compactMap { line -> String? in
                    guard let colon = line.firstIndex(of: ":") else { return nil }
                    let key = line[..<colon].trimmingCharacters(in: .whitespaces).lowercased()
                    guard key == "x-steve-debug-token" else { return nil }
                    return line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
                }.first ?? ""

                if !constantTimeEquals(suppliedHeaderToken, expectedToken) {
                    let forbidden = Data("{\"ok\":false,\"error\":\"forbidden\"}".utf8)
                    let head = "HTTP/1.1 403 Forbidden\r\nContent-Type: application/json\r\nContent-Length: \(forbidden.count)\r\nConnection: close\r\n\r\n"
                    conn.send(content: Data(head.utf8) + forbidden, completion: .contentProcessed { _ in conn.cancel() })
                    return
                }

                let bodyData = Data(raw[split.upperBound...].utf8)
                let req = (try? JSONSerialization.jsonObject(with: bodyData) as? [String: Any]) ?? [:]
                Task { @MainActor in
                    let resp = await handle(req, snapshotDir: snapshotDir, coordinator: coordinator, attention: attention)
                    let payload = (try? JSONSerialization.data(withJSONObject: resp)) ?? Data("{}".utf8)
                    let head = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: \(payload.count)\r\nConnection: close\r\n\r\n"
                    conn.send(content: Data(head.utf8) + payload, completion: .contentProcessed { _ in conn.cancel() })
                }
            }
        }
        l.start(queue: DispatchQueue(label: "app.steve.mac.debug"))
        listener = l
    }

    private static func constantTimeEquals(_ lhs: String, _ rhs: String) -> Bool {
        let a = Array(lhs.utf8)
        let b = Array(rhs.utf8)
        guard a.count == b.count else { return false }
        var diff: UInt8 = 0
        for idx in 0..<a.count {
            diff |= (a[idx] ^ b[idx])
        }
        return diff == 0
    }

    /// A snapshot may only be written inside `STEVE_CLIENT_DEBUG_DIR`. Symbolic links are resolved on
    /// both sides before the comparison, so a link inside the directory cannot lead a write out of it.
    private static func validatedSnapshotURL(path: String, allowedDir: String?) -> URL? {
        guard let allowedDir, !allowedDir.isEmpty else { return nil }
        let baseURL = URL(fileURLWithPath: allowedDir, isDirectory: true)
            .standardizedFileURL.resolvingSymlinksInPath()
        let requested = URL(fileURLWithPath: path, isDirectory: false).standardizedFileURL
        guard requested.pathExtension.lowercased() == "png" else { return nil }
        let parent = requested.deletingLastPathComponent().resolvingSymlinksInPath()
        let fileURL = parent
            .appendingPathComponent(requested.lastPathComponent, isDirectory: false)
            .resolvingSymlinksInPath()
        guard fileURL.pathExtension.lowercased() == "png" else { return nil }
        let basePrefix = baseURL.path.hasSuffix("/") ? baseURL.path : (baseURL.path + "/")
        guard fileURL.path.hasPrefix(basePrefix) else { return nil }
        return fileURL
    }

    private static func collectButtons(in view: NSView) -> [NSButton] {
        var result: [NSButton] = []
        if let btn = view as? NSButton {
            result.append(btn)
        }
        for sub in view.subviews {
            result.append(contentsOf: collectButtons(in: sub))
        }
        return result
    }

    @MainActor
    private static func handle(
        _ req: [String: Any],
        snapshotDir: String?,
        coordinator: SteveClientCoordinator,
        attention: MacAttentionController
    ) async -> [String: Any] {
        let cmd = req["cmd"] as? String ?? "status"
        switch cmd {
        case "status":
            let winIDs = NSApplication.shared.windows.map { [
                "number": $0.windowNumber,
                "title": $0.title,
                "visible": $0.isVisible,
                "isSheet": $0.isSheet,
                "identifier": $0.identifier?.rawValue ?? "",
                "miniaturized": $0.isMiniaturized,
                "appearance": $0.effectiveAppearance.name.rawValue,
                "background": $0.backgroundColor.description,
                "sheetCount": $0.sheets.count,
                "class": String(describing: type(of: $0)),
            ] as [String: Any] }
            return [
                "ok": true,
                "gateway": coordinator.localGatewayURL?.absoluteString as Any,
                "activeHubID": coordinator.hubStore.activeHubID,
                "phase": coordinator.summary.phase.rawValue,
                "pendingPair": coordinator.pendingExternalPairConfirmation?.displayName as Any,
                "pendingEndpoints": coordinator.pendingExternalPairConfirmation?.endpointsSummary as Any,
                "pairAlertPresented": coordinator.isPairConfirmationPresented,
                "pairAlertOnScreen": MacPairConfirmation.isAlertOnScreen(),
                "mainWindowCount": MacPairConfirmation.mainWindows.count,
                "pairError": coordinator.externalPairErrorMessage as Any,
                "windows": winIDs,
                "attentionScope": attention.state.scope.map { ["hubID": $0.hubProfileID, "userID": $0.userID, "workspaceID": $0.workspaceID] } as Any,
                "attentionCategories": Dictionary(uniqueKeysWithValues: AttentionKind.allCases.map { kind in
                    (kind.rawValue, ["phase": attention.state.categories[kind]?.phase.rawValue ?? "unknown",
                                    "count": attention.state.categories[kind]?.lastSuccess?.count as Any,
                                    "unit": kind.unit.rawValue] as [String: Any])
                }),
                "attentionDedup": attention.state.dedupStatus.rawValue,
                "attentionError": attention.error as Any,
                "shellAppearance": coordinator.shellContext?.appearance.map { ["dark": $0.dark, "background": $0.background, "followSystem": $0.followSystem] as [String: Any] } as Any,
                "shellContext": coordinator.shellContext.map { context in
                    ["hubID": context.hubID, "workspaceID": context.workspaceID as Any,
                     "workspaceName": context.workspaceName as Any, "route": context.route,
                     "title": context.title, "ready": context.ready, "authenticated": context.authenticated,
                     "focused": context.focused, "visible": context.visible] as [String: Any]
                } as Any,
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
            guard let path = req["path"] as? String,
                  let outURL = validatedSnapshotURL(path: path, allowedDir: snapshotDir) else {
                return ["ok": false, "error": "invalid or unauthorized snapshot path"]
            }
            let target = (req["target"] as? String) ?? "webview"
            if target == "window" {
                guard let window = MacMainWindow.windows.first(where: { $0.isVisible }),
                      let content = window.contentView,
                      let root = content.superview ?? window.contentView,
                      let bitmap = root.bitmapImageRepForCachingDisplay(in: root.bounds) else {
                    return ["ok": false, "error": "no visible main window"]
                }
                root.cacheDisplay(in: root.bounds, to: bitmap)
                guard let png = bitmap.representation(using: .png, properties: [:]) else {
                    return ["ok": false, "error": "png failed"]
                }
                do { try png.write(to: outURL) }
                catch { return ["ok": false, "error": "snapshot write failed"] }
                return ["ok": true, "windowNumber": window.windowNumber]
            }
            if target == "alert" {
                let candidateWindows = NSApplication.shared.windows.flatMap { [$0] + $0.sheets }
                guard let sheetWin = candidateWindows.first(where: {
                    $0.isSheet || String(describing: type(of: $0)).contains("Alert")
                }), let root = sheetWin.contentView,
                      let rep = root.bitmapImageRepForCachingDisplay(in: root.bounds) else {
                    return ["ok": false, "error": "no alert sheet view"]
                }
                root.cacheDisplay(in: root.bounds, to: rep)
                guard let png = rep.representation(using: .png, properties: [:]) else {
                    return ["ok": false, "error": "png failed"]
                }
                try? png.write(to: outURL)
                return ["ok": true, "windowNumber": sheetWin.windowNumber]
            }
            if ["menubar", "settings", "attention"].contains(target) {
                // Render production native roots without keyboard automation or permission prompts.
                let preview: AnyView
                switch target {
                case "settings":
                    preview = AnyView(MacClientSettingsView(attention: attention))
                case "attention":
                    preview = AnyView(MacAttentionView(coordinator: coordinator, attention: attention) { route in
                        coordinator.navigate(hashRoute: route)
                    }.padding(16).frame(width: 330))
                default:
                    preview = AnyView(MacMenuBarPopoverView(coordinator: coordinator, attention: attention))
                }
                let host = NSHostingView(
                    rootView: preview.background(Color(nsColor: .windowBackgroundColor))
                )
                let size = host.fittingSize
                host.frame = NSRect(origin: .zero, size: NSSize(width: max(size.width, 320), height: max(size.height, 360)))
                let win = NSWindow(
                    contentRect: host.frame,
                    styleMask: [.titled, .fullSizeContentView],
                    backing: .buffered,
                    defer: false
                )
                win.title = "Steve \(target)"
                win.isOpaque = true
                win.backgroundColor = .windowBackgroundColor
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
                try? png.write(to: outURL)
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
                try png.write(to: outURL)
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
                _ = win.makeFirstResponder(wv)
                wv.mouseDown(with: down)
                wv.mouseUp(with: up)
                return ["ok": true]
            }
            return ["ok": false, "error": "mouseEvent failed"]
        case "clickAlertButton":
            let wantedRole = (req["role"] as? String) ?? "confirm"
            let candidateWindows = NSApplication.shared.windows.flatMap { [$0] + $0.sheets }
            var alertButtons: [(NSWindow, NSButton)] = []
            var seen = Set<ObjectIdentifier>()
            for w in candidateWindows where w.isSheet || String(describing: type(of: w)).contains("Alert") || String(describing: type(of: w)).contains("Panel") {
                guard seen.insert(ObjectIdentifier(w)).inserted,
                      let root = w.contentView else { continue }
                for b in collectButtons(in: root) where !b.isHidden && b.isEnabled {
                    alertButtons.append((w, b))
                }
            }
            let titles = alertButtons.map { $0.1.title }
            let chosen = alertButtons.first { _, btn in
                if wantedRole == "cancel" {
                    return btn.title == "取消" || btn.title == "Cancel" || btn.keyEquivalent == "\u{1b}"
                } else {
                    return btn.title == "确认配对" || btn.title == "Pair" || btn.keyEquivalent == "\r"
                }
            }
            guard let (sheetWin, button) = chosen else {
                return ["ok": false, "error": "alert button not found", "seenButtons": titles]
            }
            let title = button.title
            let winPt = button.convert(NSPoint(x: button.bounds.midX, y: button.bounds.midY), to: nil)
            let now = ProcessInfo.processInfo.systemUptime
            let method = (req["method"] as? String) ?? "mouse"
            if method == "accessibility" {
                let pressed = button.accessibilityPerformPress()
                return [
                    "ok": pressed,
                    "method": "accessibilityPerformPress",
                    "clickedTitle": title,
                    "windowClass": String(describing: type(of: sheetWin)),
                    "seenButtons": titles,
                ]
            } else if method == "performClick" {
                button.performClick(nil)
                return [
                    "ok": true,
                    "method": "performClick",
                    "clickedTitle": title,
                    "windowClass": String(describing: type(of: sheetWin)),
                    "seenButtons": titles,
                ]
            } else if let down = NSEvent.mouseEvent(
                with: .leftMouseDown, location: winPt, modifierFlags: [],
                timestamp: now, windowNumber: sheetWin.windowNumber, context: nil,
                eventNumber: 1, clickCount: 1, pressure: 1.0
            ), let up = NSEvent.mouseEvent(
                with: .leftMouseUp, location: winPt, modifierFlags: [],
                timestamp: now + 0.02, windowNumber: sheetWin.windowNumber, context: nil,
                eventNumber: 2, clickCount: 1, pressure: 0.0
            ) {
                sheetWin.makeKeyAndOrderFront(nil)
                sheetWin.sendEvent(down)
                sheetWin.sendEvent(up)
                return [
                    "ok": true,
                    "method": "mouseEvent",
                    "clickedTitle": title,
                    "windowClass": String(describing: type(of: sheetWin)),
                    "seenButtons": titles,
                ]
            } else {
                let pressed = button.accessibilityPerformPress()
                return [
                    "ok": pressed,
                    "method": "accessibilityPerformPressFallback",
                    "clickedTitle": title,
                    "windowClass": String(describing: type(of: sheetWin)),
                    "seenButtons": titles,
                ]
            }
        case "pressEscape":
            // A real Escape key event for the alert sheet, sent the way the application sends a key
            // press (`via: "app"`, the default) or straight to the sheet (`via: "window"`).
            let candidateWindows = NSApplication.shared.windows.flatMap { [$0] + $0.sheets }
            guard let sheetWin = candidateWindows.first(where: {
                $0.isVisible && ($0.isSheet || String(describing: type(of: $0)).contains("Alert"))
            }) else {
                return ["ok": false, "error": "no alert sheet"]
            }
            let now = ProcessInfo.processInfo.systemUptime
            guard let down = NSEvent.keyEvent(
                with: .keyDown, location: .zero, modifierFlags: [], timestamp: now,
                windowNumber: sheetWin.windowNumber, context: nil,
                characters: "\u{1b}", charactersIgnoringModifiers: "\u{1b}", isARepeat: false, keyCode: 53
            ), let up = NSEvent.keyEvent(
                with: .keyUp, location: .zero, modifierFlags: [], timestamp: now + 0.02,
                windowNumber: sheetWin.windowNumber, context: nil,
                characters: "\u{1b}", charactersIgnoringModifiers: "\u{1b}", isARepeat: false, keyCode: 53
            ) else {
                return ["ok": false, "error": "keyEvent failed"]
            }
            let via = (req["via"] as? String) ?? "app"
            sheetWin.makeKeyAndOrderFront(nil)
            if via == "window" {
                sheetWin.sendEvent(down)
                sheetWin.sendEvent(up)
            } else {
                NSApplication.shared.sendEvent(down)
                NSApplication.shared.sendEvent(up)
            }
            return [
                "ok": true,
                "via": via,
                "windowClass": String(describing: type(of: sheetWin)),
                "sheetWasKey": sheetWin.isKeyWindow,
            ]
        case "openURLEvent":
            // Hands a URL to the application delegate, where AppKit delivers a `steve://` link after
            // decoding the open-URL event, so the window group's routing of it can be observed.
            guard let raw = req["uri"] as? String, let url = URL(string: raw),
                  let delegate = NSApplication.shared.delegate,
                  delegate.responds(to: #selector(NSApplicationDelegate.application(_:open:))) else {
                return ["ok": false, "error": "no url or the delegate takes no URLs"]
            }
            delegate.application?(NSApplication.shared, open: [url])
            return ["ok": true]
        case "setAppearance":
            guard let dark = req["dark"] as? Bool else { return ["ok": false, "error": "missing appearance"] }
            NSApplication.shared.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
            return ["ok": true]
        case "resizeMainWindow":
            guard let width = req["width"] as? Double, let height = req["height"] as? Double,
                  (640...2400).contains(width), (480...1800).contains(height),
                  let window = MacMainWindow.windows.first else { return ["ok": false, "error": "invalid window size"] }
            window.setContentSize(NSSize(width: width, height: height))
            return ["ok": true]
        case "restoreMainWindow":
            MacMainWindow.bringUpMainWindow()
            return ["ok": true]
        case "miniaturizeMainWindow":
            MacMainWindow.windows.first?.miniaturize(nil)
            return ["ok": true]
        case "attentionRefresh":
            attention.monitor.refresh()
            return ["ok": true]
        case "closeMainWindows":
            let mains = MacPairConfirmation.mainWindows
            mains.forEach { $0.close() }
            return ["ok": true, "closed": mains.count]
        case "openURI":
            if let uri = req["uri"] as? String {
                coordinator.requestExternalPairingConfirmation(uri: uri)
            }
            return ["ok": true]
        case "navigate":
            guard let route = req["route"] as? String else { return ["ok": false, "error": "missing route"] }
            return ["ok": coordinator.navigate(route: route, workspaceID: req["workspaceID"] as? String,
                                               targetID: req["targetID"] as? String)]
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
