import Foundation
import XCTest
@testable import SteveClientKit

private final class RefusingDefaults: UserDefaults {
    var refuse = false
    override func set(_ value: Any?, forKey key: String) {
        if !refuse { super.set(value, forKey: key) }
    }
}

final class HubAliasTests: XCTestCase {
    @MainActor
    func testRenameAndClearAreLocalAndPreserveConnectionAndCredentials() async throws {
        let suite = "hub-alias-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let credentials = InMemoryCredentialStore()
        let store = HubStore(defaults: defaults, credentials: credentials, includeLocalDefault: false)
        let first = HubProfile(id: "a", name: "First", lanURLs: [URL(string: "http://127.0.0.1:19990")!], clientID: "client", userName: "User", lastKnownVersion: "1.2.3", lastConnectedAt: Date(timeIntervalSince1970: 100))
        _ = store.upsertProfile(first, credential: "test-credential")
        _ = store.upsertProfile(HubProfile(id: "b", name: "Second", lanURLs: [URL(string: "http://127.0.0.1:19991")!]))
        let coordinator = SteveClientCoordinator(hubStore: store, preferredPortDefaults: defaults, clientDescriptor: ClientDescriptor(kind: "mac", name: "Test", platform: "macOS", version: "test"))
        defer { coordinator.stop() }
        let reload = coordinator.webReloadNonce
        _ = try await coordinator.handleBridgeCall(op: "hubs.rename", args: ["id": "a", "alias": "  私人😀  "], platform: "macos")
        XCTAssertEqual(store.profiles[0].name, "私人😀")
        XCTAssertEqual(store.activeHubID, "b")
        XCTAssertEqual(coordinator.webReloadNonce, reload)
        var expected = first
        expected.name = "私人😀"
        XCTAssertEqual(store.profiles[0], expected)
        XCTAssertEqual(store.credential(for: "a"), "test-credential")
        _ = try await coordinator.handleBridgeCall(op: "hubs.rename", args: ["id": "a", "alias": ""], platform: "ios")
        XCTAssertEqual(store.profiles[0].name, "")
        XCTAssertEqual(store.activeHubID, "b")
        XCTAssertEqual(store.credential(for: "a"), "test-credential")
        let reopened = HubStore(defaults: defaults, credentials: credentials, includeLocalDefault: false)
        XCTAssertEqual(reopened.profiles[0].name, "")
        XCTAssertEqual(reopened.profiles[0].displayName, "http://127.0.0.1:19990")
        XCTAssertEqual(reopened.profiles[0].lastKnownVersion, "1.2.3")
        XCTAssertEqual(reopened.profiles[0].clientID, "client")
        XCTAssertEqual(reopened.activeHubID, "b")
        let listed = try await coordinator.handleBridgeCall(op: "hubs.list", args: [:], platform: "macos")
        let hubs = try XCTUnwrap(listed["hubs"] as? [[String: Any]])
        XCTAssertEqual(hubs.first?["alias"] as? String, "")
        XCTAssertEqual(hubs.first?["name"] as? String, "127.0.0.1")
    }

    @MainActor
    func testLimitsUnknownIDAndPersistenceFailure() throws {
        let suite = "hub-alias-\(UUID().uuidString)"
        let defaults = RefusingDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = HubStore(defaults: defaults, credentials: InMemoryCredentialStore(), includeLocalDefault: false)
        _ = store.upsertProfile(HubProfile(id: "a", name: "Old", lanURLs: [URL(string: "http://127.0.0.1:19990")!]))
        try store.renameHub(id: "a", alias: String(repeating: "😀", count: 64))
        XCTAssertThrowsError(try store.renameHub(id: "a", alias: String(repeating: "😀", count: 65)))
        XCTAssertThrowsError(try store.renameHub(id: "a", alias: "a\nb"))
        XCTAssertThrowsError(try store.renameHub(id: "missing", alias: "new"))
        let before = store.profiles
        defaults.refuse = true
        XCTAssertThrowsError(try store.renameHub(id: "a", alias: "not saved"))
        XCTAssertEqual(store.profiles, before)
    }
}
