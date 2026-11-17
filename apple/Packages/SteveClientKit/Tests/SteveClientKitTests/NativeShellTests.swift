import Foundation
import XCTest
@testable import SteveClientKit

final class NativeShellTests: XCTestCase {
    @MainActor
    private func fixture(_ test: (SteveClientCoordinator, HubStore, UUID) throws -> Void) rethrows {
        let suite = "native-shell-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = HubStore(defaults: defaults, credentials: InMemoryCredentialStore(), includeLocalDefault: false)
        _ = store.upsertProfile(HubProfile(id: "a", name: "Test", lanURLs: [URL(string: "http://127.0.0.1:19999")!]), credential: "fixture-only")
        let coordinator = SteveClientCoordinator(hubStore: store, preferredPortDefaults: defaults,
            clientDescriptor: ClientDescriptor(kind: "mac", name: "Test", platform: "macOS", version: "test"))
        defer { coordinator.stop() }
        let owner = UUID()
        coordinator.attachShell(ownerID: owner)
        try test(coordinator, store, owner)
    }

    private func context(route: String = "#/", workspace: String? = "first", ready: Bool = true,
                         authenticated: Bool = true) -> [String: Any] {
        ["workspaceID": workspace as Any? ?? NSNull(), "workspaceName": workspace as Any? ?? NSNull(),
         "route": route, "title": "Test page", "ready": ready, "authenticated": authenticated,
         "focused": true, "visible": true]
    }

    @MainActor
    private func call(_ c: SteveClientCoordinator, _ owner: UUID, _ op: String, _ args: [String: Any] = [:],
                      document: String = "document", generation: UInt64? = nil, hub: String = "a") throws -> Any {
        try c.handleShellBridgeCall(op: op, args: args, ownerID: owner, documentID: document,
            generation: generation ?? c.shellState.generation, hubID: hub)
    }

    func testRouteAllowlistAndCanonicalActions() {
        for route in ["javascript:alert(1)", "https://example.test/#/", "#//devices", "#/items/../settings", "#/items/%2e%2e/settings", "#/items/%5c", "#/items/%00", "#/items/%23", "#/unknown", "#/hub/delete", "#/hub/add?run=1", "#/auto/acc/old", "#/pair?code=secret"] {
            XCTAssertNil(NativeShellRoute.validated(route), route)
        }
        XCTAssertEqual(NativeShellRoute.validated("/inbox"), "#/inbox")
        XCTAssertEqual(NativeShellRoute.destination("#/hub/add"), "#/")
        XCTAssertEqual(NativeShellRoute.destination("#/hub/pair"), "#/")
        XCTAssertEqual(NativeShellRoute.destination("#/settings/hub"), "#/devices/hub")
        XCTAssertEqual(NativeShellRoute.destination("#/settings/access"), "#/devices/clients")
    }

    func testChromeRequiresExplicitMacOptIn() {
        XCTAssertFalse(SteveClientCoordinator.bridgeCapabilities(for: "macos").contains("shell.chrome.hub"))
        XCTAssertTrue(SteveClientCoordinator.bridgeCapabilities(for: "macos", nativeShellChromeEnabled: true).contains("shell.chrome.hub"))
        XCTAssertFalse(SteveClientCoordinator.bridgeCapabilities(for: "ios", nativeShellChromeEnabled: true).contains("shell.chrome.hub"))
    }

    @MainActor
    func testScopedIntentRequiresReadyIssuedMatchingSuccessfulAck() throws {
        try fixture { c, _, owner in
            XCTAssertTrue(c.navigate(route: "#/inbox", workspaceID: "second"))
            XCTAssertTrue(try call(c, owner, "shell.navigation.next") is NSNull)
            _ = try call(c, owner, "shell.context", context())
            let intent = try XCTUnwrap(try call(c, owner, "shell.navigation.next") as? [String: Any])
            XCTAssertNil(intent["intent"])
            XCTAssertEqual(intent["hubID"] as? String, "a")
            let id = try XCTUnwrap(intent["id"] as? String)
            XCTAssertThrowsError(try call(c, owner, "shell.navigation.ack", ["id": id, "ok": true]))
            _ = try call(c, owner, "shell.context", context(route: "#/inbox", workspace: "second", ready: false))
            XCTAssertThrowsError(try call(c, owner, "shell.navigation.ack", ["id": id, "ok": true]))
            _ = try call(c, owner, "shell.context", context(route: "#/inbox", workspace: "second"))
            XCTAssertThrowsError(try call(c, owner, "shell.navigation.ack", ["id": "wrong", "ok": true]))
            _ = try call(c, owner, "shell.navigation.ack", ["id": id, "ok": true])
            XCTAssertTrue(c.shellState.intents.isEmpty)
            XCTAssertThrowsError(try call(c, owner, "shell.navigation.ack", ["id": id, "ok": true]))
            XCTAssertEqual(c.shellContext?.workspaceID, "second")
        }
    }

    @MainActor
    func testModalAndAliasDestinationsAndUnsupportedTargets() throws {
        try fixture { c, _, owner in
            for (route, destination) in [("#/hub/add", "#/"), ("#/hub/pair", "#/"), ("#/settings/hub", "#/devices/hub"), ("#/settings/access", "#/devices/clients")] {
                _ = try call(c, owner, "shell.context", context())
                XCTAssertTrue(c.navigate(route: route))
                let intent = try XCTUnwrap(try call(c, owner, "shell.navigation.next") as? [String: Any])
                _ = try call(c, owner, "shell.context", context(route: destination))
                _ = try call(c, owner, "shell.navigation.ack", ["id": intent["id"]!, "ok": true])
                XCTAssertTrue(c.shellState.intents.isEmpty)
            }
            XCTAssertTrue(c.navigate(route: "#/", targetID: "unsupported-reply"))
            let intent = try XCTUnwrap(try call(c, owner, "shell.navigation.next") as? [String: Any])
            XCTAssertThrowsError(try call(c, owner, "shell.navigation.ack", ["id": intent["id"]!, "ok": true]))
            _ = try call(c, owner, "shell.navigation.ack", ["id": intent["id"]!, "ok": false, "error": "Deep targets unavailable"])
            XCTAssertEqual(c.shellNavigationError, "Deep targets unavailable")
            XCTAssertTrue(try call(c, owner, "shell.navigation.next") is NSNull)
            c.dismissShellNavigationError()
            XCTAssertTrue(c.shellState.intents.isEmpty)
        }
    }

    @MainActor
    func testUnscopedAckCannotCrossWorkspaceAndReloadRejectsOldAck() throws {
        try fixture { c, _, owner in
            _ = try call(c, owner, "shell.context", context())
            c.navigate(hashRoute: "#/feed")
            let intent = try XCTUnwrap(try call(c, owner, "shell.navigation.next") as? [String: Any])
            _ = try call(c, owner, "shell.context", context(route: "#/feed", workspace: "other"))
            XCTAssertThrowsError(try call(c, owner, "shell.navigation.ack", ["id": intent["id"]!, "ok": true]))
            let generation = c.shellState.generation
            c.beginShellDocument(ownerID: owner)
            XCTAssertNil(c.shellContext)
            XCTAssertThrowsError(try call(c, owner, "shell.context", context(), generation: generation))
            _ = try call(c, owner, "shell.context", context(), document: "replacement")
            XCTAssertThrowsError(try call(c, owner, "shell.navigation.ack", ["id": intent["id"]!, "ok": true], document: "replacement"))
        }
    }

    @MainActor
    func testOldOwnerDocumentHubAndAuthReportsAreRejected() throws {
        try fixture { c, store, owner in
            _ = try call(c, owner, "shell.context", context())
            XCTAssertEqual(c.shellContext?.hubID, "a")
            XCTAssertThrowsError(try call(c, UUID(), "shell.context", context()))
            XCTAssertThrowsError(try call(c, owner, "shell.context", context(), document: "old"))
            XCTAssertThrowsError(try call(c, owner, "shell.context", context(), hub: "old-hub"))
            c.navigate(hashRoute: "#/inbox")
            let generation = c.shellState.generation
            store.updateAuthMetadata(id: "a", clientID: nil, userName: nil, credential: nil)
            c.invalidateShellScope(clearNavigation: true, keepDocument: true)
            XCTAssertThrowsError(try call(c, owner, "shell.context", context(), generation: generation))
            XCTAssertThrowsError(try call(c, owner, "shell.context", context()))
            let nonce = c.webReloadNonce
            _ = try call(c, owner, "shell.context", context(workspace: nil, ready: false, authenticated: false))
            XCTAssertFalse(c.shellContext!.authenticated)
            XCTAssertTrue(c.shellState.intents.isEmpty)
            XCTAssertEqual(c.webReloadNonce, nonce, "auth invalidation must not create reload loops")
            c.attachShell(ownerID: UUID())
            XCTAssertNil(c.shellContext)
            XCTAssertThrowsError(try call(c, owner, "shell.context", context(workspace: nil, ready: false, authenticated: false)))
        }
    }

    @MainActor
    func testHubSwitchRequiresScopedApprovalBeforeMutatingHubOrProxy() throws {
        try fixture { c, store, owner in
            _ = store.upsertProfile(HubProfile(id: "b", name: "Other", lanURLs: [URL(string: "http://127.0.0.1:19998")!]), makeActive: false)
            let token = c.proxy.proxyAuthToken
            c.switchToHub(id: "b") // No context report yet must still require approval.
            XCTAssertEqual(store.activeHubID, "a")
            XCTAssertEqual(c.proxy.proxyAuthToken, token)
            let request = try XCTUnwrap(try call(c, owner, "shell.hub-switch.next") as? [String: Any])
            let id = try XCTUnwrap(request["id"] as? String)
            XCTAssertEqual(request["hubID"] as? String, "b")
            XCTAssertThrowsError(try call(c, owner, "shell.hub-switch.ack", ["id": "wrong", "approved": true]))
            _ = try call(c, owner, "shell.hub-switch.ack", ["id": id, "approved": false])
            XCTAssertEqual(store.activeHubID, "a")
            XCTAssertEqual(c.proxy.proxyAuthToken, token)
            XCTAssertNil(c.shellState.hubSwitch)

            c.switchToHub(id: "b")
            let approved = try XCTUnwrap(try call(c, owner, "shell.hub-switch.next") as? [String: Any])
            _ = try call(c, owner, "shell.hub-switch.ack", ["id": approved["id"]!, "approved": true])
            XCTAssertEqual(store.activeHubID, "b")
        }
    }

    @MainActor
    func testHubSwitchApprovalExpiresAndCannotCrossDocumentOrOwner() throws {
        try fixture { c, store, owner in
            _ = store.upsertProfile(HubProfile(id: "b", name: "Other", lanURLs: [URL(string: "http://127.0.0.1:19998")!]), makeActive: false)
            c.switchToHub(id: "b")
            let first = try XCTUnwrap(try call(c, owner, "shell.hub-switch.next") as? [String: Any])
            let id = try XCTUnwrap(first["id"] as? String)
            XCTAssertThrowsError(try call(c, UUID(), "shell.hub-switch.ack", ["id": id, "approved": true]))
            XCTAssertThrowsError(try call(c, owner, "shell.hub-switch.ack", ["id": id, "approved": true], document: "other"))
            c.expireHubSwitchApproval(id: id)
            XCTAssertEqual(store.activeHubID, "a")
            XCTAssertNotNil(c.shellNavigationError)
            XCTAssertThrowsError(try call(c, owner, "shell.hub-switch.ack", ["id": id, "approved": true]))
            c.switchToHub(id: "b")
            let pending = try XCTUnwrap(try call(c, owner, "shell.hub-switch.next") as? [String: Any])
            c.beginShellDocument(ownerID: owner)
            XCTAssertThrowsError(try call(c, owner, "shell.hub-switch.ack", ["id": pending["id"]!, "approved": true], document: "replacement"))
            XCTAssertEqual(store.activeHubID, "a")
            c.detachShell(ownerID: UUID())
            XCTAssertEqual(c.shellState.ownerID, owner)
            c.detachShell(ownerID: owner)
            XCTAssertNil(c.shellState.ownerID)
            c.switchToHub(id: "b") // Truly detached windows have no active editor to approve.
            XCTAssertEqual(store.activeHubID, "b")
        }
    }

    @MainActor
    func testMalformedContextAndLegacyFallback() throws {
        try fixture { c, _, owner in
            var malformed = context()
            malformed["ready"] = 1
            XCTAssertThrowsError(try call(c, owner, "shell.context", malformed))
            malformed = context(); malformed.removeValue(forKey: "workspaceID")
            XCTAssertThrowsError(try call(c, owner, "shell.context", malformed))
            XCTAssertThrowsError(try call(c, owner, "shell.context", context(workspace: nil)))
            XCTAssertFalse(c.navigate(route: "javascript:bad"))
            c.navigate(hashRoute: "#/devices")
            XCTAssertEqual(c.targetHashRoute, "#/devices")
            let id = try XCTUnwrap(c.shellState.intents.first?.id)
            c.completeLegacyNavigation(id: "stale")
            XCTAssertEqual(c.shellState.intents.count, 1)
            c.completeLegacyNavigation(id: id)
            XCTAssertNil(c.targetHashRoute)
            c.navigate(hashRoute: "#/inbox")
            _ = try call(c, owner, "shell.context", context())
            XCTAssertNil(c.targetHashRoute, "acknowledged and legacy navigation must not race")
        }
    }
}
