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
    func testGeneratedLocalNameFollowsLanguageButRenameAndClearRemoveProvenance() throws {
        let suite = "hub-alias-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let language = NativeUILanguageState.shared
        defer { language.accept(nil) }
        language.accept(.english)
        let profile = HubProfile.makeLocalDefault(port: 19990)
        XCTAssertEqual(profile.nameSource, .generatedLocalDefault)
        XCTAssertEqual(profile.displayName, "Local Hub")
        let store = HubStore(defaults: defaults, credentials: InMemoryCredentialStore(), defaultProfile: profile)
        language.accept(.chinese)
        XCTAssertEqual(store.activeProfile.displayName, "本机 Hub")
        XCTAssertEqual(store.activeProfile.name, profile.name, "UI translation does not rewrite persisted data")
        XCTAssertEqual(L10n.tr("中文", "English"), "中文")
        XCTAssertEqual(HubRouteKind.loopback.displayTitle, "本机")
        let decoded = try JSONDecoder().decode(HubProfile.self, from: JSONEncoder().encode(store.activeProfile))
        XCTAssertEqual(decoded.nameSource, .generatedLocalDefault)
        try store.renameHub(id: profile.id, alias: "Local Hub")
        XCTAssertNil(store.activeProfile.nameSource)
        XCTAssertEqual(store.activeProfile.displayName, "Local Hub", "Even a default-looking user alias stays literal")
        _ = store.upsertProfile(HubProfile.makeLocalDefault(port: 19990))
        XCTAssertNil(store.activeProfile.nameSource, "Rediscovery cannot restore removed provenance")
        try store.renameHub(id: profile.id, alias: "")
        XCTAssertNil(store.activeProfile.nameSource)
        language.accept(.english)
        XCTAssertEqual(store.activeProfile.displayName, "http://127.0.0.1:19990")
        let reopened = HubStore(defaults: defaults, credentials: InMemoryCredentialStore(), defaultProfile: profile)
        XCTAssertNil(reopened.activeProfile.nameSource)
        XCTAssertEqual(reopened.activeProfile.name, "")
        XCTAssertEqual(reopened.activeProfile.displayName, "http://127.0.0.1:19990")
        for address in ["http://127.0.0.1:19990", "https://hub.example.test"] {
            let discovered = LocalHubDiscovery.defaultProfile(environment: ["STEVE_HUB_URL": address, "STEVE_PORT": "19990"])
            XCTAssertEqual(discovered.nameSource, .generatedLocalDefault)
        }
    }

    @MainActor
    func testUserAliasesAndClearedNamesAreNeverReplacedByDiscovery() throws {
        let language = NativeUILanguageState.shared
        defer { language.accept(nil) }
        language.accept(.chinese)
        for alias in ["Local Hub", "本机 Hub", "Custom", ""] {
            let suite = "hub-alias-\(UUID().uuidString)"
            let defaults = UserDefaults(suiteName: suite)!
            defer { defaults.removePersistentDomain(forName: suite) }
            let aliased = HubProfile(id: HubProfile.localDefaultID, name: alias,
                lanURLs: [URL(string: "http://127.0.0.1:19990")!], isLocalDefault: true)
            let data = try JSONEncoder().encode([aliased])
            let json = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [[String: Any]])
            XCTAssertNil(json[0]["nameSource"], "A user alias has no generated provenance")
            defaults.set(data, forKey: "steve.apple.hubs")
            let store = HubStore(defaults: defaults, credentials: InMemoryCredentialStore(),
                defaultProfile: HubProfile.makeLocalDefault(port: 19990))
            XCTAssertEqual(store.activeProfile.name, alias)
            XCTAssertNil(store.activeProfile.nameSource)
            XCTAssertEqual(store.activeProfile.displayName, alias.isEmpty ? "http://127.0.0.1:19990" : alias)
            _ = store.upsertProfile(HubProfile.makeLocalDefault(port: 19990))
            XCTAssertEqual(store.activeProfile.name, alias)
            XCTAssertNil(store.activeProfile.nameSource)
        }
    }

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
