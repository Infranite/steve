import Combine
import Foundation
import XCTest
@testable import SteveClientKit

final class NativeShellTests: XCTestCase {
    @MainActor
    private func fixture(languageEnabled: Bool = false, languageState: NativeUILanguageState? = nil,
                         _ test: (SteveClientCoordinator, HubStore, UUID) throws -> Void) rethrows {
        let suite = "native-shell-\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = HubStore(defaults: defaults, credentials: InMemoryCredentialStore(), includeLocalDefault: false)
        _ = store.upsertProfile(HubProfile(id: "a", name: "Test", lanURLs: [URL(string: "http://127.0.0.1:19999")!]), credential: "fixture-only")
        let coordinator = SteveClientCoordinator(hubStore: store, preferredPortDefaults: defaults,
            clientDescriptor: ClientDescriptor(kind: "mac", name: "Test", platform: "macOS", version: "test"),
            nativeShellLanguageEnabled: languageEnabled)
        if let languageState { coordinator.shellLanguageState = languageState }
        defer { coordinator.stop() }
        let owner = UUID()
        coordinator.attachShell(ownerID: owner)
        try test(coordinator, store, owner)
    }

    private func context(route: String = "#/", workspace: String? = "first", ready: Bool = true,
                         authenticated: Bool = true) -> [String: Any] {
        ["workspaceID": workspace as Any? ?? NSNull(), "workspaceName": workspace as Any? ?? NSNull(),
         "route": route, "title": "Test page", "ready": ready, "authenticated": authenticated,
         "focused": true, "visible": true, "language": "en"]
    }

    @MainActor
    private func call(_ c: SteveClientCoordinator, _ owner: UUID, _ op: String, _ args: [String: Any] = [:],
                      document: String = "document", generation: UInt64? = nil, hub: String = "a") throws -> Any {
        try c.handleShellBridgeCall(op: op, args: args, ownerID: owner, documentID: document,
            generation: generation ?? c.shellState.generation, hubID: hub)
    }

    @MainActor
    func testLanguageSnapshotFallbackAndPublishesOnlyEffectiveChanges() {
        let snapshot = NativeUILanguageSnapshot(systemLanguages: { ["en-US", "zh-CN"] })
        let state = NativeUILanguageState(snapshot: snapshot)
        var values: [Bool] = []
        let subscription = state.$prefersChinese.sink { values.append($0) }
        state.accept(.english)
        state.accept(nil)
        XCTAssertEqual(values, [false])
        state.accept(.chinese)
        XCTAssertTrue(snapshot.prefersChinese, "Synchronous readers change before UI publication")
        state.accept(.chinese)
        state.accept(nil)
        XCTAssertEqual(values, [false, true, false])
        withExtendedLifetime(subscription) {}
        XCTAssertTrue(NativeUILanguageSnapshot(systemLanguages: { ["zh-Hant", "en"] }).prefersChinese)
        XCTAssertTrue(NativeUILanguageSnapshot(systemLanguages: { [] }).prefersChinese)
        XCTAssertFalse(NativeUILanguageSnapshot(systemLanguages: { ["fr-FR", "zh-CN"] }).prefersChinese)
    }

    func testLanguageSnapshotHasSynchronousConcurrentReaders() {
        let snapshot = NativeUILanguageSnapshot(systemLanguages: { ["en"] })
        DispatchQueue.concurrentPerform(iterations: 100) { index in
            _ = snapshot.set(index.isMultiple(of: 2) ? .chinese : .english)
            _ = snapshot.prefersChinese
        }
        _ = snapshot.set(.chinese)
        XCTAssertTrue(snapshot.prefersChinese)
    }

    @MainActor
    func testLanguageRequiresOptInAndDoesNotChangeOSAcceptLanguageHeader() throws {
        let snapshot = NativeUILanguageSnapshot(systemLanguages: { ["en"] })
        let state = NativeUILanguageState(snapshot: snapshot)
        let header = L10n.acceptLanguageHeader
        try fixture(languageState: state) { c, _, owner in
            var args = context(); args["language"] = "zh-CN"
            _ = try call(c, owner, "shell.context", args)
            XCTAssertEqual(c.shellContext?.language, .chinese)
            XCTAssertFalse(state.prefersChinese, "Shared clients, including iOS, do not opt into a UI override")
        }
        XCTAssertEqual(L10n.acceptLanguageHeader, header)
    }

    #if os(macOS)
    @MainActor
    func testCurrentDocumentLanguageSwitchAndTransientRetention() throws {
        let snapshot = NativeUILanguageSnapshot(systemLanguages: { ["en-US"] })
        let state = NativeUILanguageState(snapshot: snapshot)
        try fixture(languageEnabled: true, languageState: state) { c, _, owner in
            var args = context(); args["language"] = "zh-CN"
            _ = try call(c, owner, "shell.context", args)
            XCTAssertTrue(state.prefersChinese)
            XCTAssertEqual(c.shellContext?.language, .chinese)
            let generation = c.shellState.generation
            c.beginShellDocument(ownerID: owner)
            XCTAssertNil(c.shellContext)
            XCTAssertTrue(state.prefersChinese, "Reload does not flash back to the OS language")
            XCTAssertThrowsError(try call(c, owner, "shell.context", context(), generation: generation))
            XCTAssertTrue(state.prefersChinese)
            _ = try call(c, owner, "shell.context", context(), document: "replacement")
            XCTAssertEqual(c.shellContext?.language, .english)
            XCTAssertFalse(state.prefersChinese, "The current document's language wins")
            _ = try call(c, owner, "shell.context", args, document: "replacement")
            c.invalidateShellScope(clearNavigation: true, keepDocument: true)
            XCTAssertTrue(state.prefersChinese)
            args["language"] = "en"
            _ = try call(c, owner, "shell.context", args, document: "replacement")
            XCTAssertFalse(state.prefersChinese)
            args["language"] = "zh-CN"
            _ = try call(c, owner, "shell.context", args, document: "replacement")
            c.detachShell(ownerID: owner)
            XCTAssertTrue(state.prefersChinese, "Closing the main window keeps MenuBar and Settings in the last language")
            XCTAssertThrowsError(try call(c, owner, "shell.context", context(), document: "replacement"))
            XCTAssertTrue(state.prefersChinese)
        }
    }

    @MainActor
    func testMalformedOrUntrustedLanguageCannotMutateContextOrLanguage() throws {
        let snapshot = NativeUILanguageSnapshot(systemLanguages: { ["en"] })
        let state = NativeUILanguageState(snapshot: snapshot)
        try fixture(languageEnabled: true, languageState: state) { c, store, owner in
            var args = context(); args["language"] = "zh-CN"
            _ = try call(c, owner, "shell.context", args)
            let accepted = c.shellContext
            for invalid in [NSNull(), true, 1, [], ["language": "en"], "", "zh", "zh-cn", "en-US", "fr", " en"] as [Any] {
                args["language"] = invalid
                args["title"] = "Must not replace context"
                XCTAssertThrowsError(try call(c, owner, "shell.context", args), "\(invalid)")
                XCTAssertEqual(c.shellContext, accepted)
                XCTAssertTrue(state.prefersChinese)
            }
            args = context(); args["language"] = "en"
            XCTAssertThrowsError(try call(c, UUID(), "shell.context", args))
            XCTAssertThrowsError(try call(c, owner, "shell.context", args, document: "old"))
            XCTAssertThrowsError(try call(c, owner, "shell.context", args, hub: "old-hub"))
            var invalidAppearance = args; invalidAppearance["appearance"] = ["dark": true]
            XCTAssertThrowsError(try call(c, owner, "shell.context", invalidAppearance))
            var invalidWorkspace = args; invalidWorkspace["workspaceID"] = NSNull()
            XCTAssertThrowsError(try call(c, owner, "shell.context", invalidWorkspace))
            store.updateAuthMetadata(id: "a", clientID: nil, userName: nil, credential: nil)
            XCTAssertThrowsError(try call(c, owner, "shell.context", args))
            XCTAssertEqual(c.shellContext, accepted)
            XCTAssertTrue(state.prefersChinese)
            c.beginShellDocument(ownerID: owner)
            args["language"] = NSNull()
            XCTAssertThrowsError(try call(c, owner, "shell.context", args, document: "invalid"))
            XCTAssertNil(c.shellState.documentID, "Invalid metadata cannot claim the new document")
            XCTAssertNil(c.shellContext)
            XCTAssertTrue(state.prefersChinese)
        }
    }
    #endif

    func testRouteAllowlistAndCanonicalActions() {
        for route in ["javascript:alert(1)", "https://example.test/#/", "#//devices", "#/items/../settings", "#/items/%2e%2e/settings", "#/items/%5c", "#/items/%00", "#/items/%23", "#/unknown", "#/hub/delete", "#/hub/add?run=1", "#/pair?code=secret"] {
            XCTAssertNil(NativeShellRoute.validated(route), route)
        }
        XCTAssertEqual(NativeShellRoute.validated("/inbox"), "#/inbox")
        XCTAssertEqual(NativeShellRoute.destination("#/hub/add"), "#/")
        XCTAssertEqual(NativeShellRoute.destination("#/hub/pair"), "#/")
        XCTAssertEqual(NativeShellRoute.destination("#/devices/hub"), "#/devices/hub")
    }

    func testChromeRequiresExplicitMacOptIn() {
        XCTAssertFalse(SteveClientCoordinator.bridgeCapabilities(for: "macos").contains("shell.chrome.hub"))
        XCTAssertTrue(SteveClientCoordinator.bridgeCapabilities(for: "macos", nativeShellChromeEnabled: true).contains("shell.chrome.hub"))
        XCTAssertFalse(SteveClientCoordinator.bridgeCapabilities(for: "ios", nativeShellChromeEnabled: true).contains("shell.chrome.hub"))
    }

    @MainActor
    func testAppearanceIsOptionalValidatedAndScopedWithContext() throws {
        try fixture { c, _, owner in
            _ = try call(c, owner, "shell.context", context())
            XCTAssertNil(c.shellContext?.appearance, "A page that has not measured its appearance stays valid")
            var args = context()
            args["appearance"] = ["dark": true, "background": [0, 127, 255]] as [String: Any]
            _ = try call(c, owner, "shell.context", args)
            XCTAssertEqual(c.shellContext?.appearance?.dark, true)
            XCTAssertEqual(c.shellContext?.appearance?.background, [0, 127, 255])
            XCTAssertEqual(c.shellContext?.appearance?.followSystem, false, "Earlier appearance payloads preserve explicit native scheme behavior")
            args["appearance"] = ["dark": false, "background": [255.0, 128.0, 0.0], "followSystem": true] as [String: Any]
            _ = try call(c, owner, "shell.context", args)
            XCTAssertEqual(c.shellContext?.appearance?.dark, false)
            XCTAssertEqual(c.shellContext?.appearance?.background, [255, 128, 0])
            XCTAssertEqual(c.shellContext?.appearance?.followSystem, true)
            _ = try call(c, owner, "shell.context", context())
            XCTAssertNil(c.shellContext?.appearance, "Missing metadata must not retain an earlier appearance")
            _ = try call(c, owner, "shell.context", args)
            let generation = c.shellState.generation
            c.beginShellDocument(ownerID: owner)
            XCTAssertNil(c.shellContext, "Appearance has no state outside the scoped context")
            XCTAssertThrowsError(try call(c, owner, "shell.context", args, generation: generation))
            _ = try call(c, owner, "shell.context", args, document: "replacement")
            c.detachShell(ownerID: owner)
            XCTAssertNil(c.shellContext)
        }
    }

    @MainActor
    func testMalformedAppearanceRejectsWholeContextWithoutReplacingValidMetadata() throws {
        try fixture { c, _, owner in
            var args = context()
            args["appearance"] = ["dark": true, "background": [10, 20, 30]] as [String: Any]
            _ = try call(c, owner, "shell.context", args)
            let accepted = c.shellContext
            let invalid: [Any] = [
                NSNull(), true, [], "dark", [:] as [String: Any],
                ["dark": true], ["background": [1, 2, 3]],
                ["dark": 1, "background": [1, 2, 3]],
                ["dark": true, "background": [1, 2, 3], "followSystem": 1],
                ["dark": true, "background": [1, 2, 3], "followSystem": NSNull()],
                ["dark": true, "background": [1, 2, 3], "followSystem": "true"],
                ["dark": "true", "background": [1, 2, 3]],
                ["dark": true, "background": [1, 2, 3], "extra": true],
                ["dark": true, "background": "1,2,3"],
                ["dark": true, "background": [1, 2]],
                ["dark": true, "background": [1, 2, 3, 4]],
                ["dark": true, "background": [true, 2, 3] as [Any]],
                ["dark": true, "background": ["1", 2, 3] as [Any]],
                ["dark": true, "background": [NSNull(), 2, 3] as [Any]],
                ["dark": true, "background": [-1, 2, 3]],
                ["dark": true, "background": [256, 2, 3]],
                ["dark": true, "background": [1.5, 2, 3]],
                ["dark": true, "background": [Double.nan, 2, 3]],
                ["dark": true, "background": [Double.infinity, 2, 3]],
            ]
            for appearance in invalid {
                args["appearance"] = appearance
                args["title"] = "Must not replace the accepted context"
                XCTAssertThrowsError(try call(c, owner, "shell.context", args), "\(appearance)")
                XCTAssertEqual(c.shellContext, accepted)
            }
        }
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
    func testModalDestinationsAndUnsupportedTargets() throws {
        try fixture { c, _, owner in
            for (route, destination) in [("#/hub/add", "#/"), ("#/hub/pair", "#/")] {
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

    private func visibility(_ target: AttentionTarget, workspace: String = "first", visible: Bool = true) -> [String: Any] {
        ["workspaceID": workspace, "targets": [["destination": target.destination.rawValue, "entityID": target.entityID]],
         "focused": true, "visible": visible]
    }

    @MainActor
    func testDeepTargetsRequireTypedRouteAndActuallyVisibleCurrentDocument() throws {
        try fixture { c, _, owner in
            for destination in [AttentionTarget.Destination.reply, .pending, .feed, .channel] {
                let target = try AttentionTarget(destination: destination, entityID: "target:one &two")
                _ = try call(c, owner, "shell.context", context())
                XCTAssertTrue(c.navigate(route: target.nativeRoute, workspaceID: "first", targetID: target.entityID))
                let intent = try XCTUnwrap(try call(c, owner, "shell.navigation.next") as? [String: Any])
                _ = try call(c, owner, "shell.context", context(route: target.nativeRoute))
                XCTAssertThrowsError(try call(c, owner, "shell.navigation.ack", ["id": intent["id"]!, "ok": true]))
                _ = try call(c, owner, "shell.attention.visible", visibility(target, visible: false))
                XCTAssertThrowsError(try call(c, owner, "shell.navigation.ack", ["id": intent["id"]!, "ok": true]))
                let wrongType = try AttentionTarget(destination: destination == .reply ? .pending : .reply, entityID: target.entityID)
                _ = try call(c, owner, "shell.attention.visible", visibility(wrongType))
                XCTAssertThrowsError(try call(c, owner, "shell.navigation.ack", ["id": intent["id"]!, "ok": true]))
                _ = try call(c, owner, "shell.attention.visible", visibility(target))
                _ = try call(c, owner, "shell.navigation.ack", ["id": intent["id"]!, "ok": true])
                XCTAssertTrue(c.shellState.intents.isEmpty)
            }
        }
    }

    @MainActor
    func testReloadIsTransientAndForegroundRequiresExactVisibleFocusedScope() throws {
        try fixture { c, _, owner in
            let target = try AttentionTarget(destination: .reply, entityID: "reply1")
            let other = try AttentionTarget(destination: .reply, entityID: "reply2")
            let scope = AttentionScope(hubProfileID: "a", userID: "native-verified-user", workspaceID: "first")
            XCTAssertEqual(NativeAttentionContextUpdate.classify(nil, hubID: "a"), .transient)
            _ = try call(c, owner, "shell.context", context(workspace: nil, ready: false, authenticated: false))
            XCTAssertEqual(NativeAttentionContextUpdate.classify(c.shellContext, hubID: "a"), .transient)
            _ = try call(c, owner, "shell.context", context())
            XCTAssertEqual(NativeAttentionContextUpdate.classify(c.shellContext, hubID: "a"), .workspace(id: "first", name: "first"))
            _ = try call(c, owner, "shell.attention.visible", visibility(target))
            let report = try XCTUnwrap(c.shellAttentionVisibility)
            XCTAssertTrue(report.activelyViews(target, scope: scope, context: c.shellContext, nativeFocused: true))
            XCTAssertFalse(report.activelyViews(other, scope: scope, context: c.shellContext, nativeFocused: true))
            XCTAssertFalse(report.activelyViews(target, scope: scope, context: c.shellContext, nativeFocused: false))
            XCTAssertFalse(report.activelyViews(target, scope: .init(hubProfileID: "a", userID: scope.userID, workspaceID: "other"), context: c.shellContext, nativeFocused: true))
            var blurred = context(); blurred["focused"] = false
            _ = try call(c, owner, "shell.context", blurred)
            XCTAssertFalse(report.activelyViews(target, scope: scope, context: c.shellContext, nativeFocused: true))
            c.beginShellDocument(ownerID: owner)
            XCTAssertFalse(report.activelyViews(target, scope: scope, context: c.shellContext, nativeFocused: true))
            _ = try call(c, owner, "shell.context", context(workspace: nil, authenticated: false), document: "replacement")
            XCTAssertEqual(NativeAttentionContextUpdate.classify(c.shellContext, hubID: "a"), .signedOut)
        }
    }

    @MainActor
    func testVisibilityScopeRejectsWrongWorkspaceMalformedAndStaleReports() throws {
        try fixture { c, _, owner in
            let target = try AttentionTarget(destination: .reply, entityID: "reply1")
            XCTAssertThrowsError(try call(c, owner, "shell.attention.visible", visibility(target)))
            _ = try call(c, owner, "shell.context", context())
            XCTAssertThrowsError(try call(c, owner, "shell.attention.visible", visibility(target, workspace: "other")))
            var malformed = visibility(target); malformed["focused"] = 1
            XCTAssertThrowsError(try call(c, owner, "shell.attention.visible", malformed))
            malformed = visibility(target); malformed["targets"] = [["destination": "script", "entityID": "id"]]
            XCTAssertThrowsError(try call(c, owner, "shell.attention.visible", malformed))
            _ = try call(c, owner, "shell.attention.visible", visibility(target))
            XCTAssertEqual(c.shellAttentionVisibility?.hubID, "a")
            XCTAssertEqual(c.shellAttentionVisibility?.targets, [target])
            _ = try call(c, owner, "shell.context", context(route: "#/inbox"))
            XCTAssertNil(c.shellAttentionVisibility)
            _ = try call(c, owner, "shell.attention.visible", visibility(target))
            let generation = c.shellState.generation
            c.beginShellDocument(ownerID: owner)
            XCTAssertNil(c.shellAttentionVisibility)
            XCTAssertThrowsError(try call(c, owner, "shell.attention.visible", visibility(target), generation: generation))
            _ = try call(c, owner, "shell.context", context(workspace: nil, ready: true, authenticated: false), document: "replacement")
            XCTAssertThrowsError(try call(c, owner, "shell.attention.visible", visibility(target), document: "replacement"))
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
    func testMalformedContextIsRejected() throws {
        try fixture { c, _, owner in
            var malformed = context()
            malformed["ready"] = 1
            XCTAssertThrowsError(try call(c, owner, "shell.context", malformed))
            malformed = context(); malformed.removeValue(forKey: "workspaceID")
            XCTAssertThrowsError(try call(c, owner, "shell.context", malformed))
            malformed = context(); malformed.removeValue(forKey: "language")
            XCTAssertThrowsError(try call(c, owner, "shell.context", malformed))
            XCTAssertThrowsError(try call(c, owner, "shell.context", context(workspace: nil)))
            XCTAssertFalse(c.navigate(route: "javascript:bad"))
        }
    }
}
