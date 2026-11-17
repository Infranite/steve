import Foundation
import CoreFoundation

/// Page metadata only. Native assigns Hub, generation and document identity after WebView verification.
public struct NativeShellContext: Equatable, Sendable {
    public let hubID: String
    public let generation: UInt64
    public let documentID: String
    public let workspaceID: String?
    public let workspaceName: String?
    public let route: String
    public let title: String
    public let ready: Bool
    public let authenticated: Bool
    public let focused: Bool
    public let visible: Bool
}

public struct NativeNavigationIntent: Equatable, Sendable, Identifiable {
    public let id: String
    public let hubID: String
    public let workspaceID: String?
    public let route: String
    public let targetID: String?

    var bridgeValue: [String: Any] {
        var value: [String: Any] = ["id": id, "hubID": hubID, "route": route]
        if let workspaceID { value["workspaceID"] = workspaceID }
        if let targetID { value["targetID"] = targetID }
        return value
    }
}

public enum NativeShellRoute {
    public static func validated(_ raw: String) -> String? {
        let route = raw.hasPrefix("/") ? "#" + raw : raw
        guard route.hasPrefix("#/"), route.utf8.count <= 4096,
              let decoded = route.dropFirst().removingPercentEncoding,
              !decoded.contains("#"), !decoded.contains("\\"),
              !decoded.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else { return nil }
        let path = decoded.components(separatedBy: "?")[0]
        let parts = path.dropFirst().components(separatedBy: "/")
        let roots: Set<String> = ["", "c", "feed", "inbox", "items", "projects", "sessions", "settings", "bots", "capabilities", "design", "knowledge", "devices", "github", "integrations", "channels", "auto", "signals", "mail", "accounts", "vault", "yolo", "stats", "contacts", "orgs", "teams", "publications", "more", "hub"]
        guard let root = parts.first, roots.contains(root), !path.contains("//"),
              !parts.contains("."), !parts.contains("..") else { return nil }
        if root == "hub" && (parts.count != 2 || !["add", "pair"].contains(parts[1]) || decoded.contains("?")) { return nil }
        if root == "auto" && parts.count > 1 && ["acc", "space"].contains(parts[1]) { return nil }
        return route
    }

    public static func destination(_ route: String) -> String {
        switch route {
        case "#/hub/add", "#/hub/pair": return "#/"
        case "#/settings/hub": return "#/devices/hub"
        case "#/settings/access": return "#/devices/clients"
        default: return route
        }
    }
}

struct NativeHubSwitchRequest {
    let id = UUID().uuidString
    let hubID: String
    var issuedDocumentID: String?
}

struct NativeShellState {
    var hubSwitch: NativeHubSwitchRequest?
    var hubSwitchTimeout: Task<Void, Never>?
    var ownerID: UUID?
    var documentID: String?
    var generation: UInt64 = 0
    var intents: [NativeNavigationIntent] = []
    var issuedID: String?
    var issuedWorkspaceID: String?
    var failedID: String?
}

@MainActor
extension SteveClientCoordinator {
    func attachShell(ownerID: UUID) {
        invalidateShellScope(clearNavigation: false)
        shellState.ownerID = ownerID
    }

    func detachShell(ownerID: UUID) {
        guard shellState.ownerID == ownerID else { return }
        invalidateShellScope(clearNavigation: false)
        shellState.ownerID = nil
    }

    private func cancelHubSwitchApproval() {
        shellState.hubSwitchTimeout?.cancel()
        shellState.hubSwitchTimeout = nil
        shellState.hubSwitch = nil
    }

    func requestHubSwitchApproval(id: String) {
        // An attached page must explicitly approve, even while loading/offline or missing context.
        // Absence of a report is not evidence that no editor/write is active.
        guard shellState.ownerID != nil else { performApprovedHubSwitch(id: id); return }
        cancelHubSwitchApproval()
        let request = NativeHubSwitchRequest(hubID: id)
        shellState.hubSwitch = request
        shellState.hubSwitchTimeout = Task { @MainActor [weak self] in
            do { try await Task.sleep(for: .seconds(30)) } catch { return }
            self?.expireHubSwitchApproval(id: request.id)
        }
    }

    func expireHubSwitchApproval(id: String) {
        guard shellState.hubSwitch?.id == id else { return }
        cancelHubSwitchApproval()
        shellNavigationError = L10n.tr("页面未确认切换 Hub，请重试。", "The page did not approve switching Hubs. Please try again.")
    }

    func invalidateShellScope(clearNavigation: Bool, keepDocument: Bool = false) {
        cancelHubSwitchApproval()
        shellState.generation &+= 1
        if !keepDocument { shellState.documentID = nil }
        shellContext = nil
        shellState.issuedID = nil
        shellState.issuedWorkspaceID = nil
        if clearNavigation {
            shellState.intents.removeAll()
            shellState.failedID = nil
            shellNavigationError = nil
            targetHashRoute = nil
        }
    }

    func beginShellDocument(ownerID: UUID) {
        guard shellState.ownerID == ownerID else { return }
        invalidateShellScope(clearNavigation: false)
    }

    /// Only SteveWebView calls this, after verifying current main frame, WebView and document identity.
    func handleShellBridgeCall(op: String, args: [String: Any], ownerID: UUID,
                               documentID: String, generation: UInt64, hubID: String) throws -> Any {
        guard shellState.ownerID == ownerID, shellState.generation == generation,
              hubStore.activeHubID == hubID,
              shellState.documentID == nil || shellState.documentID == documentID else {
            throw BridgeCallError.operationFailed("Stale native shell document")
        }
        shellState.documentID = documentID
        switch op {
        case "shell.hub-switch.next":
            guard var request = shellState.hubSwitch else { return NSNull() }
            request.issuedDocumentID = documentID
            shellState.hubSwitch = request
            return ["id": request.id, "hubID": request.hubID]
        case "shell.hub-switch.ack":
            guard let id = args["id"] as? String, let approved = Self.shellBoolean(args["approved"]),
                  let request = shellState.hubSwitch, request.id == id,
                  request.issuedDocumentID == documentID else {
                throw BridgeCallError.invalidArgument("Unknown or stale Hub switch approval")
            }
            cancelHubSwitchApproval()
            if approved { performApprovedHubSwitch(id: request.hubID) }
            return [String: Any]()
        case "shell.context":
            guard let rawRoute = args["route"] as? String, let route = NativeShellRoute.validated(rawRoute),
                  let title = args["title"] as? String, title.count <= 512,
                  let ready = Self.shellBoolean(args["ready"]),
                  let authenticated = Self.shellBoolean(args["authenticated"]),
                  let focused = Self.shellBoolean(args["focused"]), let visible = Self.shellBoolean(args["visible"]),
                  Self.nullableShellString(args["workspaceID"]), Self.nullableShellString(args["workspaceName"]) else {
                throw BridgeCallError.invalidArgument("Invalid native shell context")
            }
            let workspace = (args["workspaceID"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            // Account changes invalidate the old reports without forcing a reload of the sign-in page.
            let nativeAuthenticated = !(hubStore.credential(for: hubID) ?? "").isEmpty
            guard !authenticated || nativeAuthenticated, !ready || !authenticated || workspace != nil,
                  authenticated || workspace == nil else {
                throw BridgeCallError.invalidArgument("Context authentication or workspace is stale")
            }
            if shellContext?.authenticated == true && !authenticated {
                invalidateShellScope(clearNavigation: true, keepDocument: true)
            }
            shellContext = NativeShellContext(hubID: hubID, generation: shellState.generation, documentID: documentID,
                workspaceID: workspace, workspaceName: workspace == nil ? nil : (args["workspaceName"] as? String).map(Self.shellText),
                route: route, title: Self.shellText(title), ready: ready, authenticated: authenticated,
                focused: focused, visible: visible)
            targetHashRoute = nil // This page uses acknowledged navigation, never also inject a hash.
            return [String: Any]()
        case "shell.navigation.next":
            guard let context = shellContext, context.ready,
                  let intent = shellState.intents.first, intent.hubID == hubID,
                  context.authenticated || intent.route.hasPrefix("#/hub/"),
                  shellState.failedID != intent.id else { return NSNull() }
            if shellState.issuedID != intent.id { shellState.issuedWorkspaceID = context.workspaceID }
            shellState.issuedID = intent.id
            return intent.bridgeValue // Direct intent or null, not an envelope.
        case "shell.navigation.ack":
            guard let id = args["id"] as? String, let ok = Self.shellBoolean(args["ok"]),
                  let intent = shellState.intents.first, intent.id == id, shellState.issuedID == id,
                  let context = shellContext, context.documentID == documentID, context.hubID == hubID else {
                throw BridgeCallError.invalidArgument("Unknown or stale navigation acknowledgement")
            }
            if ok {
                guard context.ready, context.authenticated || intent.route == "#/hub/add", intent.targetID == nil,
                      context.route == NativeShellRoute.destination(intent.route),
                      context.workspaceID == (intent.workspaceID ?? shellState.issuedWorkspaceID) else {
                    throw BridgeCallError.invalidArgument("Navigation has not reached its target")
                }
                shellState.intents.removeFirst()
                shellState.issuedID = nil
                shellState.issuedWorkspaceID = nil
                shellState.failedID = nil
                shellNavigationError = nil
            } else {
                shellState.failedID = id // Failed intents stay visible, but are not retried forever.
                shellNavigationError = Self.shellText(String((args["error"] as? String ?? "Navigation failed").prefix(512)))
            }
            return [String: Any]()
        default: throw BridgeCallError.unsupportedOperation(op)
        }
    }

    @discardableResult
    public func navigate(route: String, workspaceID: String? = nil, targetID: String? = nil) -> Bool {
        guard let route = NativeShellRoute.validated(route), !hubStore.activeHubID.isEmpty else {
            shellNavigationError = L10n.tr("不支持的导航目标", "Unsupported navigation target")
            return false
        }
        if let failed = shellState.failedID { shellState.intents.removeAll { $0.id == failed } }
        shellState.failedID = nil
        shellNavigationError = nil
        shellState.intents.append(NativeNavigationIntent(id: UUID().uuidString, hubID: hubStore.activeHubID,
            workspaceID: workspaceID, route: route, targetID: targetID))
        targetHashRoute = shellContext == nil ? shellState.intents.first?.route : nil
        return true
    }

    public func dismissShellNavigationError() {
        if let failed = shellState.failedID { shellState.intents.removeAll { $0.id == failed } }
        shellState.failedID = nil
        shellState.issuedID = nil
        shellState.issuedWorkspaceID = nil
        shellNavigationError = nil
    }

    func completeLegacyNavigation(id: String) {
        guard shellContext == nil, shellState.intents.first?.id == id else { return }
        shellState.intents.removeFirst()
        targetHashRoute = shellState.intents.first?.route
    }

    private static func nullableShellString(_ value: Any?) -> Bool {
        value is NSNull || (value as? String).map { $0.count <= 512 } == true
    }
    private static func shellBoolean(_ value: Any?) -> Bool? {
        guard let value = value as? NSNumber, CFGetTypeID(value) == CFBooleanGetTypeID() else { return nil }
        return value.boolValue
    }
    private static func shellText(_ value: String) -> String {
        String(value.unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) &&
            ![0x202A, 0x202B, 0x202C, 0x202D, 0x202E, 0x2066, 0x2067, 0x2068, 0x2069].contains($0.value) })
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }
}
