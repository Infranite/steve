import Foundation
import Network
import XCTest
@testable import SteveClientKit

final class SteveClientKitTests: XCTestCase {
    func testPairingPayloadParsesPublicAndLanEndpointsAndPrefersPublic() {
        let uri =
            "steve://pair?v=1&code=stp_0123456789abcdef0123456789abcdef"
            + "&name=Studio%20Mac"
            + "&pub=https%3A%2F%2Fsteve.example.com%2F"
            + "&lan=http%3A%2F%2F192.168.1.40%3A8440"
            + "&lan=http%3A%2F%2F100.64.0.12%3A8440"

        let parsed = PairingPayload.parse(uri)
        XCTAssertNotNil(parsed)
        XCTAssertEqual(parsed?.code, "stp_0123456789abcdef0123456789abcdef")
        XCTAssertEqual(parsed?.name, "Studio Mac")
        XCTAssertEqual(parsed?.publicURL?.absoluteString, "https://steve.example.com")
        XCTAssertEqual(parsed?.lanURLs.map(\.absoluteString), [
            "http://192.168.1.40:8440",
            "http://100.64.0.12:8440",
        ])
        XCTAssertTrue(parsed?.usesPublicByDefault == true)
        XCTAssertEqual(parsed?.orderedEndpoints.map(\.absoluteString), [
            "https://steve.example.com",
            "http://192.168.1.40:8440",
            "http://100.64.0.12:8440",
        ])
    }

    func testPairingPayloadWithoutPublicUsesLanDirectly() {
        let uri = "steve://pair?v=1&code=stp_abcdef0123456789abcdef0123456789&lan=http%3A%2F%2F192.168.1.88%3A8440"
        let parsed = PairingPayload.parse(uri)
        XCTAssertNotNil(parsed)
        XCTAssertNil(parsed?.publicURL)
        XCTAssertEqual(parsed?.usesPublicByDefault, false)
        XCTAssertEqual(parsed?.orderedEndpoints.map(\.absoluteString), ["http://192.168.1.88:8440"])
    }

    func testPairingPayloadRejectsInvalidSchemesOrShortCodes() {
        XCTAssertNil(PairingPayload.parse("https://steve.example.com/pair?code=stp_0123456789abcdef"))
        XCTAssertNil(PairingPayload.parse("steve://pair?v=1&code=short&lan=http://192.168.1.2:8440"))
        XCTAssertNil(PairingPayload.parse("steve://pair?v=1&code=stp_0123456789abcdef")) // no endpoints
    }

    // Review #1: Real Hub `/auth/pair` and `/auth/token` responses carry `user: {id, name}` (no `role`) and `hub: {version, api}`.
    func testPairSignInResponseDecodesRealHubShapeWithoutRole() throws {
        let realHubJSON = Data("""
        {
          "client_id": "cli_0123456789ab",
          "credential": "cli_0123456789ab.secret_token",
          "user": { "id": "u_owner", "name": "Owner" },
          "hub": { "version": "0.3.0", "api": 1 }
        }
        """.utf8)
        let decoded = try JSONDecoder().decode(PairSignInResponse.self, from: realHubJSON)
        XCTAssertEqual(decoded.client_id, "cli_0123456789ab")
        XCTAssertEqual(decoded.credential, "cli_0123456789ab.secret_token")
        XCTAssertEqual(decoded.user.id, "u_owner")
        XCTAssertEqual(decoded.user.name, "Owner")
        XCTAssertEqual(decoded.hub.version, "0.3.0")
        XCTAssertEqual(decoded.hub.api, 1)
    }

    // Review #12 & #14: Public-first routing, concurrent LAN probing, and API version check.
    func testEndpointResolverConnectsViaPublicRacesLanConcurrentlyAndChecksAPIVersion() async throws {
        let pub = URL(string: "https://steve.example.com")!
        let lan = URL(string: "http://192.168.1.20:8440")!
        let hello = HubHelloResponse(name: "steve", version: "0.3.0", api: 1, login: .init(token: true, oidc: false))

        actor AttemptRecorder {
            var seen: [String] = []
            func record(_ url: URL) { seen.append(url.absoluteString) }
            func list() -> [String] { seen }
        }

        // 1. Public reachable -> connects via public without touching LAN
        let rec1 = AttemptRecorder()
        let resolverBothUp = EndpointResolver { candidate in
            await rec1.record(candidate)
            return hello
        }
        let res1 = try await resolverBothUp.resolve(publicURL: pub, lanURLs: [lan])
        XCTAssertEqual(res1.url, pub)
        XCTAssertEqual(res1.route, .publicNetwork)
        XCTAssertFalse(res1.usedFallbackFromPublic)
        let seen1 = await rec1.list()
        XCTAssertEqual(seen1, ["https://steve.example.com"])

        // 2. Public unreachable + 3 slow/dead LAN bridges + 1 fast LAN IP -> finishes concurrently in < 0.8s
        let slow1 = URL(string: "http://172.17.0.1:8440")!
        let slow2 = URL(string: "http://10.211.55.2:8440")!
        let slow3 = URL(string: "http://198.18.0.1:8440")!
        let resolverRace = EndpointResolver { candidate in
            if candidate == pub {
                throw URLError(.cannotConnectToHost)
            }
            if candidate != lan {
                try await Task.sleep(nanoseconds: 2_500_000_000)
                throw URLError(.timedOut)
            }
            return hello
        }
        let start = Date()
        let res2 = try await resolverRace.resolve(publicURL: pub, lanURLs: [slow1, slow2, slow3, lan])
        let elapsed = Date().timeIntervalSince(start)
        XCTAssertEqual(res2.url, lan)
        XCTAssertEqual(res2.route, .lan)
        XCTAssertTrue(res2.usedFallbackFromPublic)
        XCTAssertLessThan(elapsed, 1.0, "LAN candidates must be probed concurrently rather than sequentially")

        // 3. Incompatible API version surfaces `.incompatibleAPI`
        let resolverFutureAPI = EndpointResolver { _ in
            HubHelloResponse(name: "steve", version: "9.0.0", api: 99, login: .init(token: true, oidc: false))
        }
        do {
            _ = try await resolverFutureAPI.resolve(publicURL: nil, lanURLs: [lan])
            XCTFail("Expected incompatibleAPI error")
        } catch let err as EndpointResolutionError {
            XCTAssertEqual(err, .incompatibleAPI(version: 99))
        }
    }

    // Review #11 & #13: LocalHubDiscovery parses `steve.env` and iOS HubStore starts with no bogus 127.0.0.1:8440 profile.
    @MainActor
    func testLocalHubDiscoveryReadsSteveEnvAndIOSHubStoreStartsEmpty() throws {
        let tempHome = FileManager.default.temporaryDirectory.appendingPathComponent("steve-env-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: tempHome, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tempHome) }

        try Data("""
        # Steve configuration written by steve-install
        STEVE_HOST=0.0.0.0
        export STEVE_PORT="18540" # custom port
        """.utf8).write(to: tempHome.appendingPathComponent("steve.env"))

        XCTAssertEqual(LocalHubDiscovery.parsePort(fromEnvContents: "export STEVE_PORT='19440'\n"), 19440)
        let profile = LocalHubDiscovery.defaultProfile(environment: [:], steveHome: tempHome)
        XCTAssertEqual(profile.orderedEndpoints.map(\.absoluteString), ["http://127.0.0.1:18540"])

        // Environment variable overrides steve.env
        let envOverride = LocalHubDiscovery.defaultProfile(environment: ["STEVE_PORT": "18545"], steveHome: tempHome)
        XCTAssertEqual(envOverride.orderedEndpoints.map(\.absoluteString), ["http://127.0.0.1:18545"])

        // iOS mode (`includeLocalDefault: false`) starts clean with zero profiles
        let suiteName = "test.steve.iosstore.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let iosStore = HubStore(
            defaults: defaults,
            credentials: InMemoryCredentialStore(),
            defaultProfile: profile,
            includeLocalDefault: false
        )
        XCTAssertFalse(iosStore.hasProfiles)
        XCTAssertTrue(iosStore.profiles.isEmpty)
        XCTAssertNil(iosStore.currentProfile)
    }

    @MainActor
    func testHubStoreDefaultsToLocalHubAndSupportsAddingSwitchingAndRemovingOtherHubs() {
        let suiteName = "test.steve.hubstore.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let creds = InMemoryCredentialStore()
        let store = HubStore(
            defaults: defaults,
            credentials: creds,
            defaultProfile: LocalHubDiscovery.defaultProfile(environment: ["STEVE_PORT": "18540"]),
            includeLocalDefault: true
        )

        XCTAssertEqual(store.activeProfile.id, HubProfile.localDefaultID)
        XCTAssertTrue(store.activeProfile.isLocalDefault)
        XCTAssertEqual(store.activeProfile.orderedEndpoints.map(\.absoluteString), ["http://127.0.0.1:18540"])

        let added = store.addManualHub(address: "https://remote.example.com/", name: "Remote Hub", makeActive: true)
        XCTAssertNotNil(added)
        XCTAssertEqual(store.activeProfile.name, "Remote Hub")
        XCTAssertEqual(store.activeProfile.publicURL?.absoluteString, "https://remote.example.com")

        store.selectHub(id: HubProfile.localDefaultID)
        XCTAssertEqual(store.activeProfile.id, HubProfile.localDefaultID)

        XCTAssertNil(store.removeHub(id: HubProfile.localDefaultID))
        XCTAssertEqual(store.profiles.count, 2)
        if let remoteID = added?.id {
            store.selectHub(id: remoteID)
            let removed = store.removeHub(id: remoteID)
            XCTAssertEqual(removed?.wasActive, true)
        }
        XCTAssertEqual(store.profiles.count, 1)
        XCTAssertEqual(store.activeProfile.id, HubProfile.localDefaultID)
    }

    // Review #2 & Suspected Redirects: Loopback proxy enforces Host, Origin, and per-launch cookie secret, and does not auto-follow redirects.
    func testLoopbackProxyEnforcesHostOriginAndPerLaunchCookieSecret() async throws {
        let tempDir = FileManager.default.temporaryDirectory.appendingPathComponent("steve-proxy-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tempDir) }

        let indexHTML = "<!doctype html><html><body>Steve Native Bundle</body></html>"
        try Data(indexHTML.utf8).write(to: tempDir.appendingPathComponent("index.html"))

        let mockHub = try await MockUpstreamServer.start()
        defer { mockHub.stop() }

        let proxy = HubLoopbackProxy(webRoot: tempDir)
        proxy.updateTarget(.init(hubID: "hub-main", baseURL: mockHub.baseURL, credential: "cli_test.secret_token"))
        let gateway = try await proxy.start()
        defer { proxy.stop() }

        // 1. Static root serves bundled index.html when Host matches 127.0.0.1:<port>
        let (htmlData, htmlResp) = try await RawTCPHTTPClient.request(
            port: proxy.port,
            path: "/",
            headers: [("Host", "127.0.0.1:\(proxy.port)")]
        )
        XCTAssertEqual(htmlResp.status, 200)
        XCTAssertEqual(String(data: htmlData, encoding: .utf8), indexHTML)

        // 2. DNS-rebinding Host (`evil.example:<port>`) is rejected with 403 even on `/`
        let (_, dnsRebindResp) = try await RawTCPHTTPClient.request(
            port: proxy.port,
            path: "/",
            headers: [("Host", "evil.example:\(proxy.port)")]
        )
        XCTAssertEqual(dnsRebindResp.status, 403)

        // 3. Local process hitting `/api/me` WITHOUT `steve_proxy_key` cookie gets 403 Forbidden (no credential leak!)
        let (noCookieData, noCookieResp) = try await RawTCPHTTPClient.request(
            port: proxy.port,
            path: "/api/me",
            headers: [("Host", "127.0.0.1:\(proxy.port)")]
        )
        XCTAssertEqual(noCookieResp.status, 403)
        XCTAssertTrue(String(data: noCookieData, encoding: .utf8)?.contains("forbidden_proxy_key") == true)

        // 4. Foreign `Origin` header is rejected with 403 even if cookie were present
        let (_, badOriginResp) = try await RawTCPHTTPClient.request(
            port: proxy.port,
            path: "/api/me",
            headers: [
                ("Host", "127.0.0.1:\(proxy.port)"),
                ("Origin", "http://evil.example"),
                ("Cookie", proxy.authCookieHeader),
            ]
        )
        XCTAssertEqual(badOriginResp.status, 403)

        // 5. Legitimate WKWebView request carrying `steve_proxy_key` cookie & valid Host/Origin succeeds,
        //    injects `Authorization: Bearer`, and strips `steve_proxy_key` from upstream `Cookie`!
        var authedReq = URLRequest(url: gateway.appendingPathComponent("api/me"))
        authedReq.setValue("http://127.0.0.1:\(proxy.port)", forHTTPHeaderField: "Origin")
        authedReq.setValue(proxy.authCookieHeader, forHTTPHeaderField: "Cookie")
        let noCookieCfg = URLSessionConfiguration.ephemeral
        noCookieCfg.httpShouldSetCookies = false
        let clientSession = URLSession(configuration: noCookieCfg)
        let (apiData, apiResp) = try await clientSession.data(for: authedReq)
        XCTAssertEqual((apiResp as? HTTPURLResponse)?.statusCode, 200)
        let json = try JSONSerialization.jsonObject(with: apiData) as? [String: String]
        XCTAssertEqual(json?["auth"], "Bearer cli_test.secret_token")
        XCTAssertEqual(json?["cookie"], "")
        XCTAssertEqual(json?["path"], "/api/me")

        // 6. SSE `/api/stream` streams events incrementally with valid proxy cookie
        var sseReq = URLRequest(url: gateway.appendingPathComponent("api/stream"))
        sseReq.setValue(proxy.authCookieHeader, forHTTPHeaderField: "Cookie")
        let (bytes, sseResp) = try await clientSession.bytes(for: sseReq)
        XCTAssertEqual((sseResp as? HTTPURLResponse)?.statusCode, 200)
        XCTAssertEqual((sseResp as? HTTPURLResponse)?.value(forHTTPHeaderField: "Content-Type"), "text/event-stream")
        var receivedLines: [String] = []
        for try await line in bytes.lines {
            receivedLines.append(line)
            if receivedLines.count >= 2 { break }
        }
        XCTAssertEqual(receivedLines, ["id: 1", "data: {\"type\":\"steve.thinking\",\"on\":true}"])

        // 7. Upstream 302 redirect is returned raw without following or leaking Authorization
        let (_, redirResp) = try await RawTCPHTTPClient.request(
            port: proxy.port,
            path: "/auth/redirect-test",
            headers: [
                ("Host", "127.0.0.1:\(proxy.port)"),
                ("Cookie", proxy.authCookieHeader),
            ]
        )
        XCTAssertEqual(redirResp.status, 302)
        XCTAssertEqual(redirResp.header("location"), "https://idp.example.com/authorize")
    }

    // Review #5: Cross-Hub cookie isolation and multi-Set-Cookie splitting (including `Expires=Wed, ...`).
    func testLoopbackProxyIsolatesCookiesAcrossHubSwitches() async throws {
        let tempDir = FileManager.default.temporaryDirectory.appendingPathComponent("steve-cookies-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tempDir) }
        try Data("<html></html>".utf8).write(to: tempDir.appendingPathComponent("index.html"))

        let hubA = try await MockUpstreamServer.start()
        defer { hubA.stop() }
        let hubB = try await MockUpstreamServer.start()
        defer { hubB.stop() }

        let proxy = HubLoopbackProxy(webRoot: tempDir)
        proxy.updateTarget(.init(hubID: "hub-alpha", baseURL: hubA.baseURL, credential: nil))
        _ = try await proxy.start()
        defer { proxy.stop() }

        // Sign in on Hub A where upstream sets two cookies (including an Expires attribute containing a comma)
        let (_, loginResp) = try await RawTCPHTTPClient.request(
            port: proxy.port,
            path: "/auth/set-multi-cookie",
            headers: [
                ("Host", "127.0.0.1:\(proxy.port)"),
                ("Cookie", proxy.authCookieHeader),
            ]
        )
        XCTAssertEqual(loginResp.status, 200)
        let setCookies = loginResp.headers.filter { $0.0.lowercased() == "set-cookie" }.map(\.1)
        XCTAssertEqual(setCookies.count, 2, "Multiple upstream Set-Cookie headers must remain separate")

        let alphaPrefix = HubLoopbackProxy.hubCookiePrefix(for: "hub-alpha")
        XCTAssertTrue(setCookies.contains { $0.hasPrefix("\(alphaPrefix)steve_client=cookie_cred_a;") && !$0.lowercased().contains("secure") })
        XCTAssertTrue(setCookies.contains { $0.hasPrefix("\(alphaPrefix)steve_ws=workspace_a;") && $0.contains("SameSite=Lax") })

        // Simulate browser sending all accumulated cookies (proxy key + Hub A's cookies + a legacy unprefixed cookie)
        let browserCookieJar = [
            proxy.authCookieHeader,
            "\(alphaPrefix)steve_client=cookie_cred_a",
            "\(alphaPrefix)steve_ws=workspace_a",
            "steve_client=legacy_unscoped",
        ].joined(separator: "; ")

        // While Hub A is active, Hub A receives its unwrapped cookies (and NOT legacy_unscoped or steve_proxy_key)
        let (echoAData, _) = try await RawTCPHTTPClient.request(
            port: proxy.port,
            path: "/api/me",
            headers: [
                ("Host", "127.0.0.1:\(proxy.port)"),
                ("Cookie", browserCookieJar),
            ]
        )
        let echoA = try JSONSerialization.jsonObject(with: echoAData) as? [String: String]
        XCTAssertEqual(echoA?["cookie"], "steve_client=cookie_cred_a; steve_ws=workspace_a")

        // Switch active target to Hub B: Hub B must receive ZERO cookies from Hub A!
        proxy.updateTarget(.init(hubID: "hub-beta", baseURL: hubB.baseURL, credential: "cli_b.secret"))
        let (echoBData, _) = try await RawTCPHTTPClient.request(
            port: proxy.port,
            path: "/api/me",
            headers: [
                ("Host", "127.0.0.1:\(proxy.port)"),
                ("Cookie", browserCookieJar),
            ]
        )
        let echoB = try JSONSerialization.jsonObject(with: echoBData) as? [String: String]
        XCTAssertEqual(echoB?["cookie"], "", "Hub A cookies must never be forwarded to Hub B after switching")
        XCTAssertEqual(echoB?["auth"], "Bearer cli_b.secret")
    }

    // Review #6: Preferred loopback port persists across restarts and falls back cleanly when occupied.
    func testLoopbackProxyPersistsPreferredPortAndFallsBackWhenOccupied() async throws {
        let suiteName = "test.steve.proxyport.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let proxy1 = HubLoopbackProxy(preferredPortDefaults: defaults)
        let url1 = try await proxy1.start()
        let firstPort = proxy1.port
        XCTAssertGreaterThan(firstPort, 0)
        proxy1.stop()

        let proxy2 = HubLoopbackProxy(preferredPortDefaults: defaults)
        let url2 = try await proxy2.start()
        XCTAssertEqual(url2, url1, "Second launch should reuse persisted port to keep WKWebView localStorage origin")

        // Occupy `firstPort` with proxy2 still running; proxy3 must fall back to a different ephemeral port cleanly
        let proxy3 = HubLoopbackProxy(preferredPortDefaults: defaults)
        let url3 = try await proxy3.start()
        XCTAssertNotEqual(url3, url1)
        XCTAssertGreaterThan(proxy3.port, 0)
        proxy2.stop()
        proxy3.stop()
    }

    // Review #3: `steve://pair` confirmation gate, mandatory credential check, and credential isolation from existing profiles.
    @MainActor
    func testQRPairingConfirmationAndCredentialIsolationAgainstProfileHijacking() async throws {
        let attackerNoCredHub = try await MockUpstreamServer.start(omitPairCredential: true)
        defer { attackerNoCredHub.stop() }
        let realNewHub = try await MockUpstreamServer.start(omitPairCredential: false)
        defer { realNewHub.stop() }

        let suiteName = "test.steve.pairsec.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let creds = InMemoryCredentialStore()
        let store = HubStore(
            defaults: defaults,
            credentials: creds,
            defaultProfile: HubProfile.makeLocalDefault(port: 18540),
            includeLocalDefault: true
        )
        // Store a real owner credential on `local-default`
        store.updateAuthMetadata(
            id: HubProfile.localDefaultID,
            clientID: "cli_local_owner",
            userName: "Owner",
            credential: "cli_local_owner.top_secret"
        )

        let coordinator = SteveClientCoordinator(
            hubStore: store,
            preferredPortDefaults: defaults,
            clientDescriptor: ClientDescriptor(kind: "mac", name: "Test Mac", platform: "macOS", version: "0.3.0")
        )
        defer { coordinator.stop() }

        // 1. External `steve://pair` URL queues confirmation rather than executing immediately
        let lanLocalEncoded = "http://127.0.0.1:18540".addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)!
        let attackerPubEncoded = attackerNoCredHub.baseURL.absoluteString.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)!
        let evilURI = "steve://pair?v=1&code=stp_0123456789abcdef0123456789abcdef&name=EvilHub&pub=\(attackerPubEncoded)&lan=\(lanLocalEncoded)"

        coordinator.requestExternalPairingConfirmation(uri: evilURI)
        XCTAssertNotNil(coordinator.pendingExternalPairConfirmation)
        XCTAssertEqual(coordinator.pendingExternalPairConfirmation?.displayName, "EvilHub")
        XCTAssertTrue(coordinator.pendingExternalPairConfirmation?.endpointsSummary.contains(attackerNoCredHub.baseURL.absoluteString) == true)
        XCTAssertEqual(store.profiles.count, 1, "Must not pair before user confirms")

        // 2. Even if confirmed, an attacker server omitting `credential` fails and leaves `local-default` untouched
        await coordinator.confirmPendingExternalPairing()
        XCTAssertNotNil(coordinator.externalPairErrorMessage)
        XCTAssertEqual(store.profiles.count, 1)
        XCTAssertNil(store.activeProfile.publicURL)
        XCTAssertEqual(store.credential(for: HubProfile.localDefaultID), "cli_local_owner.top_secret")

        // 3. Even when pairing succeeds with a valid credential and overlapping `lan=http://127.0.0.1:18540`,
        //    it creates a distinct profile and NEVER overwrites `local-default` or steals its credential!
        let validPubEncoded = realNewHub.baseURL.absoluteString.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)!
        let overlappingURI = "steve://pair?v=1&code=stp_fedcba9876543210fedcba9876543210&name=PairedHub&pub=\(validPubEncoded)&lan=\(lanLocalEncoded)"
        let pairedProfile = try await coordinator.pairWithQRCode(uri: overlappingURI)
        XCTAssertNotEqual(pairedProfile.id, HubProfile.localDefaultID)
        XCTAssertEqual(store.profiles.count, 2)
        let localAfter = store.profiles.first { $0.id == HubProfile.localDefaultID }!
        XCTAssertNil(localAfter.publicURL, "local-default must never acquire an external publicURL from pairing")
        XCTAssertEqual(store.credential(for: HubProfile.localDefaultID), "cli_local_owner.top_secret")
        XCTAssertEqual(store.credential(for: pairedProfile.id), "cli_paired.secret_cred")
    }

    // Review #4: Gateway origin confinement and rewriting gateway `/api/files/...` URLs for external browser opening.
    func testGatewayOriginConfinementAndExternalURLRewriting() {
        let gateway = URL(string: "http://127.0.0.1:51234")!
        let activeHub = URL(string: "https://hub.example.com:8443")!

        XCTAssertTrue(SteveWebView.isGatewayOrigin(URL(string: "http://127.0.0.1:51234/#/inbox")!, gateway: gateway))
        XCTAssertFalse(SteveWebView.isGatewayOrigin(URL(string: "http://127.0.0.1:18540/")!, gateway: gateway))
        XCTAssertFalse(SteveWebView.isGatewayOrigin(URL(string: "http://localhost:51234/")!, gateway: gateway))
        XCTAssertFalse(SteveWebView.isGatewayOrigin(URL(string: "https://127.0.0.1:51234/")!, gateway: gateway))
        XCTAssertFalse(SteveWebView.isGatewayOrigin(URL(string: "https://github.com/org/repo")!, gateway: gateway))

        let rewrittenAttachment = SteveWebView.rewriteExternalGatewayURL(
            URL(string: "http://127.0.0.1:51234/api/files/f_123?download=1")!,
            gateway: gateway,
            activeHubEndpoint: activeHub
        )
        XCTAssertEqual(rewrittenAttachment.absoluteString, "https://hub.example.com:8443/api/files/f_123?download=1")

        let externalUnchanged = SteveWebView.rewriteExternalGatewayURL(
            URL(string: "https://github.com/org/repo/releases")!,
            gateway: gateway,
            activeHubEndpoint: activeHub
        )
        XCTAssertEqual(externalUnchanged.absoluteString, "https://github.com/org/repo/releases")
    }

    // Review #7 & #14: Stale activation race protection + removing an inactive Hub revokes client without reloading active WebView.
    @MainActor
    func testStaleActivationDoesNotOverwriteSwitchAndInactiveRemoveDoesNotReload() async throws {
        let mockHubB = try await MockUpstreamServer.start()
        defer { mockHubB.stop() }

        let suiteName = "test.steve.race.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let slowURL = URL(string: "http://192.168.99.1:8440")!
        let hello = HubHelloResponse(name: "steve", version: "0.3.0", api: 1, login: .init(token: true, oidc: false))
        let resolver = EndpointResolver { candidate in
            if candidate == slowURL {
                try await Task.sleep(nanoseconds: 350_000_000)
            }
            return hello
        }

        let creds = InMemoryCredentialStore()
        let store = HubStore(
            defaults: defaults,
            credentials: creds,
            defaultProfile: HubProfile(id: HubProfile.localDefaultID, name: "Slow A", lanURLs: [slowURL], isLocalDefault: true),
            includeLocalDefault: true
        )
        let hubB = store.addManualHub(address: mockHubB.baseURL.absoluteString, name: "Fast B", makeActive: false)!
        creds.setCredential("cli_b.secret", for: hubB.id)

        let coordinator = SteveClientCoordinator(
            hubStore: store,
            resolver: resolver,
            preferredPortDefaults: defaults,
            clientDescriptor: ClientDescriptor(kind: "mac", name: "Test Mac", platform: "macOS", version: "0.3.0")
        )
        defer { coordinator.stop() }

        // Start a background poll on Slow A, then immediately switch to Fast B before Slow A resolves
        let stalePoll = Task {
            await coordinator.activateCurrentHub(forceReloadWeb: false)
        }
        try await Task.sleep(nanoseconds: 40_000_000)
        store.selectHub(id: hubB.id)
        await coordinator.activateCurrentHub(forceReloadWeb: true)
        await stalePoll.value

        XCTAssertEqual(coordinator.proxy.currentTarget().hubID, hubB.id)
        XCTAssertEqual(coordinator.proxy.currentTarget().baseURL, mockHubB.baseURL)
        XCTAssertEqual(coordinator.summary.activeEndpoint, mockHubB.baseURL)

        // Switch back to Slow A, then remove inactive Hub B -> must NOT change `webReloadNonce`, and must call `/auth/client/logout`
        store.selectHub(id: HubProfile.localDefaultID)
        await coordinator.activateCurrentHub(forceReloadWeb: true)
        let nonceBeforeRemove = coordinator.webReloadNonce

        coordinator.removeHub(id: hubB.id)
        try await Task.sleep(nanoseconds: 150_000_000)
        XCTAssertEqual(coordinator.webReloadNonce, nonceBeforeRemove, "Removing an inactive Hub must not reload active WebView")
        XCTAssertEqual(mockHubB.lastLogoutAuth(), "Bearer cli_b.secret")
    }

    @MainActor
    func testCoordinatorQRPairingAndMenuBarStateSummaryEndToEnd() async throws {
        let mockHub = try await MockUpstreamServer.start()
        defer { mockHub.stop() }

        let suiteName = "test.steve.coordinator.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let creds = InMemoryCredentialStore()
        let store = HubStore(
            defaults: defaults,
            credentials: creds,
            defaultProfile: LocalHubDiscovery.defaultProfile(environment: ["STEVE_PORT": "18540"]),
            includeLocalDefault: true
        )
        let coordinator = SteveClientCoordinator(
            hubStore: store,
            preferredPortDefaults: defaults,
            clientDescriptor: ClientDescriptor(kind: "ios", name: "Test iPhone", platform: "iOS", version: "0.3.0")
        )
        defer { coordinator.stop() }

        // Public URL is unreachable (closed port 9), LAN URL points to mockHub -> falls back to LAN automatically
        let pubEncoded = "http://127.0.0.1:9".addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)!
        let lanEncoded = mockHub.baseURL.absoluteString.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)!
        let pairURI = "steve://pair?v=1&code=stp_0123456789abcdef0123456789abcdef&name=Office%20Mac&pub=\(pubEncoded)&lan=\(lanEncoded)"

        let profile = try await coordinator.pairWithQRCode(uri: pairURI)
        XCTAssertEqual(profile.name, "Office Mac")
        XCTAssertEqual(store.activeProfile.id, profile.id)
        XCTAssertEqual(store.credential(for: profile.id), "cli_paired.secret_cred")

        XCTAssertEqual(coordinator.summary.phase, .connected)
        XCTAssertEqual(coordinator.summary.hubVersion, "0.3.0")
        XCTAssertEqual(coordinator.summary.workspaceName, "Core")
        XCTAssertEqual(coordinator.summary.runningSessions, 2)
        XCTAssertEqual(coordinator.summary.openDecisions, 1)
        XCTAssertEqual(coordinator.summary.onlineDevices, 2)
        XCTAssertEqual(coordinator.summary.totalDevices, 3)
        XCTAssertTrue(coordinator.summary.steveThinking)
    }

    @MainActor
    func testWebSignInAndLogoutThroughProxySyncsNativeHubStoreAndSummary() async throws {
        let mockHub = try await MockUpstreamServer.start()
        defer { mockHub.stop() }

        let suiteName = "test.steve.websync.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let creds = InMemoryCredentialStore()
        let store = HubStore(
            defaults: defaults,
            credentials: creds,
            defaultProfile: HubProfile(id: HubProfile.localDefaultID, name: "Local Hub", lanURLs: [mockHub.baseURL], isLocalDefault: true),
            includeLocalDefault: true
        )
        let coordinator = SteveClientCoordinator(
            hubStore: store,
            preferredPortDefaults: defaults,
            clientDescriptor: ClientDescriptor(kind: "mac", name: "Test Mac", platform: "macOS", version: "0.3.0")
        )
        defer { coordinator.stop() }

        let gateway = try await coordinator.proxy.ensureRunning()
        await coordinator.activateCurrentHub(forceReloadWeb: true)
        XCTAssertNil(store.credential(for: HubProfile.localDefaultID))

        // Simulate Web UI submitting `POST /auth/token` through `HubLoopbackProxy`
        let (_, loginResp) = try await RawTCPHTTPClient.request(
            port: coordinator.proxy.port,
            method: "POST",
            path: "/auth/token",
            headers: [
                ("Host", "127.0.0.1:\(coordinator.proxy.port)"),
                ("Origin", gateway.absoluteString),
                ("Cookie", coordinator.proxy.authCookieHeader),
            ]
        )
        XCTAssertEqual(loginResp.status, 200)
        try await Task.sleep(nanoseconds: 120_000_000)

        XCTAssertEqual(store.credential(for: HubProfile.localDefaultID), "cli_web_tok.secret_cred")
        XCTAssertEqual(coordinator.proxy.currentTarget().credential, "cli_web_tok.secret_cred")
        XCTAssertEqual(coordinator.summary.phase, .connected)

        // Simulate Web UI logging out via `POST /auth/client/logout`
        let (_, logoutResp) = try await RawTCPHTTPClient.request(
            port: coordinator.proxy.port,
            method: "POST",
            path: "/auth/client/logout",
            headers: [
                ("Host", "127.0.0.1:\(coordinator.proxy.port)"),
                ("Origin", gateway.absoluteString),
                ("Cookie", coordinator.proxy.authCookieHeader),
            ]
        )
        XCTAssertEqual(logoutResp.status, 200)
        try await Task.sleep(nanoseconds: 120_000_000)

        XCTAssertNil(store.credential(for: HubProfile.localDefaultID))
        XCTAssertNil(coordinator.proxy.currentTarget().credential)
    }
}

private struct RawHTTPResponse {
    let status: Int
    let headers: [(String, String)]

    func header(_ name: String) -> String? {
        headers.first { $0.0.lowercased() == name.lowercased() }?.1
    }
}

private enum RawTCPHTTPClient {
    static func request(
        port: UInt16,
        method: String = "GET",
        path: String,
        headers: [(String, String)]
    ) async throws -> (Data, RawHTTPResponse) {
        let conn = NWConnection(host: .ipv4(.loopback), port: NWEndpoint.Port(rawValue: port)!, using: .tcp)
        conn.start(queue: DispatchQueue.global())
        defer { conn.cancel() }

        var rawReq = "\(method) \(path) HTTP/1.1\r\n"
        for (k, v) in headers {
            rawReq += "\(k): \(v)\r\n"
        }
        rawReq += "Connection: close\r\n\r\n"

        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            conn.send(content: Data(rawReq.utf8), completion: .contentProcessed { err in
                if let err { cont.resume(throwing: err) } else { cont.resume(returning: ()) }
            })
        }

        var buffer = Data()
        while true {
            let chunk: Data = try await withCheckedThrowingContinuation { cont in
                conn.receive(minimumIncompleteLength: 1, maximumLength: 65_536) { data, _, isComplete, err in
                    if let err {
                        cont.resume(throwing: err)
                    } else if let data, !data.isEmpty {
                        cont.resume(returning: data)
                    } else if isComplete {
                        cont.resume(returning: Data())
                    } else {
                        cont.resume(returning: Data())
                    }
                }
            }
            if chunk.isEmpty { break }
            buffer.append(chunk)
        }

        let delim = Data([0x0D, 0x0A, 0x0D, 0x0A])
        guard let range = buffer.range(of: delim),
              let headStr = String(data: buffer[..<range.lowerBound], encoding: .utf8) else {
            throw URLError(.badServerResponse)
        }
        let lines = headStr.components(separatedBy: "\r\n")
        let status = lines.first?.split(separator: " ").dropFirst().first.flatMap { Int($0) } ?? 0
        var parsedHeaders: [(String, String)] = []
        for line in lines.dropFirst() {
            guard let colon = line.firstIndex(of: ":") else { continue }
            let k = String(line[..<colon]).trimmingCharacters(in: .whitespaces)
            let v = String(line[line.index(after: colon)...]).trimmingCharacters(in: .whitespaces)
            parsedHeaders.append((k, v))
        }
        let body = Data(buffer[range.upperBound...])
        return (body, RawHTTPResponse(status: status, headers: parsedHeaders))
    }
}

private final class MockUpstreamServer: @unchecked Sendable {
    let listener: NWListener
    let baseURL: URL
    private let lock = NSLock()
    private var recordedLogoutAuth: String?

    private init(listener: NWListener, port: UInt16) {
        self.listener = listener
        self.baseURL = URL(string: "http://127.0.0.1:\(port)")!
    }

    func lastLogoutAuth() -> String? {
        lock.lock()
        defer { lock.unlock() }
        return recordedLogoutAuth
    }

    private func recordLogout(_ auth: String) {
        lock.lock()
        recordedLogoutAuth = auth
        lock.unlock()
    }

    static func start(omitPairCredential: Bool = false) async throws -> MockUpstreamServer {
        let params = NWParameters.tcp
        params.requiredLocalEndpoint = NWEndpoint.hostPort(host: .ipv4(.loopback), port: .any)
        let listener = try NWListener(using: params)

        final class Box: @unchecked Sendable {
            weak var server: MockUpstreamServer?
        }
        let box = Box()

        let port: UInt16 = try await withCheckedThrowingContinuation { cont in
            final class Once: @unchecked Sendable {
                let lock = NSLock()
                var done = false
                func fire(_ f: () -> Void) { lock.lock(); defer { lock.unlock() }; guard !done else { return }; done = true; f() }
            }
            let once = Once()
            listener.newConnectionHandler = { conn in
                conn.start(queue: DispatchQueue.global())
                conn.receive(minimumIncompleteLength: 1, maximumLength: 16_384) { data, _, _, _ in
                    let raw = String(data: data ?? Data(), encoding: .utf8) ?? ""
                    let lines = raw.components(separatedBy: "\r\n")
                    let path = lines.first?.split(separator: " ").dropFirst().first.map(String.init) ?? ""
                    let headerVal = { (prefix: String) -> String in
                        lines.first(where: { $0.lowercased().hasPrefix(prefix) })?
                            .split(separator: ":", maxSplits: 1).dropFirst().first?
                            .trimmingCharacters(in: .whitespaces) ?? ""
                    }
                    let auth = headerVal("authorization:")
                    let cookie = headerVal("cookie:")

                    if path == "/api/stream" {
                        let sseHead = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n"
                        let frame = "id: 1\ndata: {\"type\":\"steve.thinking\",\"on\":true}\n\n"
                        conn.send(content: Data((sseHead + frame).utf8), completion: .contentProcessed { _ in
                            DispatchQueue.global().asyncAfter(deadline: .now() + 1.0) { conn.cancel() }
                        })
                        return
                    }

                    if path == "/auth/redirect-test" {
                        let head = "HTTP/1.1 302 Found\r\nLocation: https://idp.example.com/authorize\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        conn.send(content: Data(head.utf8), completion: .contentProcessed { _ in conn.cancel() })
                        return
                    }

                    if path == "/auth/set-multi-cookie" {
                        let payload = Data("{\"ok\":true}".utf8)
                        let head =
                            "HTTP/1.1 200 OK\r\n"
                            + "Content-Type: application/json\r\n"
                            + "Set-Cookie: steve_client=cookie_cred_a; Path=/; HttpOnly; Secure\r\n"
                            + "Set-Cookie: steve_ws=workspace_a; Expires=Wed, 11 Nov 2026 12:00:00 GMT; Path=/; SameSite=None; Secure\r\n"
                            + "Content-Length: \(payload.count)\r\n"
                            + "Connection: close\r\n\r\n"
                        conn.send(content: Data(head.utf8) + payload, completion: .contentProcessed { _ in conn.cancel() })
                        return
                    }

                    if path == "/auth/client/logout" {
                        box.server?.recordLogout(auth)
                        let payload = Data("{\"ok\":true}".utf8)
                        let head = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: \(payload.count)\r\nConnection: close\r\n\r\n"
                        conn.send(content: Data(head.utf8) + payload, completion: .contentProcessed { _ in conn.cancel() })
                        return
                    }

                    let payload: Data
                    if path == "/auth/hello" {
                        payload = Data("{\"name\":\"steve\",\"version\":\"0.3.0\",\"api\":1,\"login\":{\"token\":true,\"oidc\":false}}".utf8)
                    } else if path == "/auth/token" {
                        payload = Data("{\"client_id\":\"cli_web_tok\",\"credential\":\"cli_web_tok.secret_cred\",\"user\":{\"id\":\"owner\",\"name\":\"Owner\"},\"hub\":{\"version\":\"0.3.0\",\"api\":1}}".utf8)
                    } else if path == "/auth/pair" {
                        if omitPairCredential {
                            payload = Data("{\"client_id\":\"cli_evil\",\"user\":{\"id\":\"owner\",\"name\":\"Owner\"},\"hub\":{\"version\":\"0.3.0\",\"api\":1}}".utf8)
                        } else {
                            // Exact real Hub shape: `user: {id, name}` (no `role`), `hub: {version, api}`
                            payload = Data("{\"client_id\":\"cli_paired\",\"credential\":\"cli_paired.secret_cred\",\"user\":{\"id\":\"owner\",\"name\":\"Owner\"},\"hub\":{\"version\":\"0.3.0\",\"api\":1}}".utf8)
                        }
                    } else if path == "/api/state" {
                        payload = Data("""
                        {"version":"0.3.0","web_version":"0.3.0","thinking":true,"workspace":{"name":"Core"},"devices":[{"status":"正常"},{"status":"暂停接活"},{"status":"离线"}],"sessions":[{"status":"running","busy":true},{"status":"idle","busy":true},{"status":"idle","busy":false}],"decisions":[{"status":"open"},{"status":"resolved"}]}
                        """.utf8)
                    } else {
                        payload = try! JSONEncoder().encode(["path": path, "auth": auth, "cookie": cookie])
                    }
                    let head = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: \(payload.count)\r\nConnection: close\r\n\r\n"
                    conn.send(content: Data(head.utf8) + payload, completion: .contentProcessed { _ in conn.cancel() })
                }
            }
            listener.stateUpdateHandler = { state in
                if case .ready = state, let p = listener.port?.rawValue, p > 0 {
                    once.fire { cont.resume(returning: p) }
                } else if case .failed(let err) = state {
                    once.fire { cont.resume(throwing: err) }
                }
            }
            listener.start(queue: DispatchQueue(label: "test.steve.mock-listener"))
        }
        let srv = MockUpstreamServer(listener: listener, port: port)
        box.server = srv
        return srv
    }

    func stop() {
        listener.cancel()
    }
}
