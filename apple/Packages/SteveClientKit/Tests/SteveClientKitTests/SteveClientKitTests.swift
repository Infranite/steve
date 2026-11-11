import Foundation
import Network
import WebKit
import XCTest
@testable import SteveClientKit

final class SteveClientKitTests: XCTestCase {
    // MARK: - Unified Pairing Link & URL Normalization

    func testPairingPayloadParsesSteveAndHTTPSLinksAndIgnoresOuterGatewayHost() {
        let steveURI =
            "steve://pair?v=1&code=stp_0123456789abcdef0123456789abcdef"
            + "&name=Studio%20Mac"
            + "&pub=https%3A%2F%2Fsteve.example.com%2F"
            + "&lan=http%3A%2F%2F192.168.1.40%3A8440,http%3A%2F%2F100.64.0.12%3A8440"

        let parsedSteve = PairingPayload.parse(steveURI)
        XCTAssertNotNil(parsedSteve)
        XCTAssertEqual(parsedSteve?.code, "stp_0123456789abcdef0123456789abcdef")
        XCTAssertEqual(parsedSteve?.name, "Studio Mac")
        XCTAssertEqual(parsedSteve?.publicURL?.absoluteString, "https://steve.example.com")
        XCTAssertEqual(parsedSteve?.lanURLs.map(\.absoluteString), [
            "http://192.168.1.40:8440",
            "http://100.64.0.12:8440",
        ])
        XCTAssertEqual(parsedSteve?.usesPublicByDefault, true)

        // Arrival link inside a native shell (`http://127.0.0.1:<gateway>/#/pair?...`) with `+` spaces and `%26`/`%2B` in name:
        // Outer `127.0.0.1:54321` must never be treated as a Hub endpoint.
        let gatewayArrivalLink =
            "http://127.0.0.1:54321/#/pair?v=1&code=stp_abcdef0123456789abcdef0123456789"
            + "&name=R%26D%2BTeam+Hub"
            + "&lan=http%3A%2F%2F192.168.1.88%3A8440"

        let parsedGateway = PairingPayload.parse(gatewayArrivalLink)
        XCTAssertNotNil(parsedGateway)
        XCTAssertEqual(parsedGateway?.name, "R&D+Team Hub")
        XCTAssertNil(parsedGateway?.publicURL)
        XCTAssertEqual(parsedGateway?.orderedEndpoints.map(\.absoluteString), ["http://192.168.1.88:8440"])

        // Public HTTPS web link (`https://hub.example.com/#/pair?...`)
        let httpsLink =
            "https://hub.example.com/#/pair?v=1&code=stp_99887766554433221100aabbccddeeff"
            + "&name=Cloud+Hub&pub=https%3A%2F%2Fhub.example.com&lan=http%3A%2F%2F10.0.0.5%3A8440"
        let parsedHTTPS = PairingPayload.parse(httpsLink)
        XCTAssertEqual(parsedHTTPS?.publicURL?.absoluteString, "https://hub.example.com")
        XCTAssertEqual(parsedHTTPS?.lanURLs.map(\.absoluteString), ["http://10.0.0.5:8440"])
    }

    func testPairingPayloadRejectsInvalidLinksAndOuterOriginWithoutEmbeddedEndpoints() {
        // Query on path instead of `#/pair?...`
        XCTAssertNil(PairingPayload.parse("https://steve.example.com/pair?code=stp_0123456789abcdef&pub=https://steve.example.com"))
        // Outer HTTP host without `pub` or `lan` in `#/pair?...` must NOT fall back to outer host
        XCTAssertNil(PairingPayload.parse("http://127.0.0.1:54321/#/pair?v=1&code=stp_0123456789abcdef"))
        // Short code
        XCTAssertNil(PairingPayload.parse("steve://pair?v=1&code=stp_short&lan=http://192.168.1.2:8440"))
        // No endpoints
        XCTAssertNil(PairingPayload.parse("steve://pair?v=1&code=stp_0123456789abcdef"))
    }

    func testURLNormalizerAndVendorNeutralPrivateOverlayClassification() {
        XCTAssertEqual(URLNormalizer.normalize("192.168.1.10:8440/")?.absoluteString, "http://192.168.1.10:8440")
        XCTAssertEqual(URLNormalizer.normalize("localhost:8440")?.absoluteString, "http://localhost:8440")
        XCTAssertEqual(URLNormalizer.normalize("https://Steve.Example.COM:443///")?.absoluteString, "https://steve.example.com")
        XCTAssertNil(URLNormalizer.normalize("ftp://steve.example.com"))
        XCTAssertNil(URLNormalizer.normalize("steve://pair?code=stp_0123456789abcdef"))
        XCTAssertNil(URLNormalizer.normalize("javascript:alert(1)"))
        XCTAssertNil(URLNormalizer.normalize("http://user:pass@192.168.1.10:8440"))

        // RFC 6598 100.64.0.0/10 -> vendor-neutral `.privateOverlay` ("overlay"), never vendor-specific `.ts.net`
        XCTAssertEqual(HubRouteKind.classify(url: URL(string: "http://100.64.0.1:8440")!), .privateOverlay)
        XCTAssertEqual(HubRouteKind.classify(url: URL(string: "http://100.127.255.254:8440")!), .privateOverlay)
        XCTAssertEqual(HubRouteKind.privateOverlay.rawValue, "overlay")
        XCTAssertEqual(HubRouteKind.classify(url: URL(string: "http://100.63.255.255:8440")!), .publicNetwork)
        XCTAssertEqual(HubRouteKind.classify(url: URL(string: "http://100.128.0.1:8440")!), .publicNetwork)
        XCTAssertEqual(HubRouteKind.classify(url: URL(string: "https://node.example.ts.net")!), .publicNetwork)

        // When a local Hub has no STEVE_PUBLIC_URL, its public_url is `http://127.0.0.1:<port>` and still classifies as `.loopback`
        let localPub = URL(string: "http://127.0.0.1:18521")!
        XCTAssertEqual(HubRouteKind.classify(url: localPub, explicitPublicURL: localPub), .loopback)
        XCTAssertEqual(HubRouteKind.classify(url: URL(string: "https://steve.localhost")!), .loopback)
        XCTAssertEqual(HubRouteKind.classify(url: URL(string: "http://nas:8440")!), .lan)
        XCTAssertEqual(HubRouteKind.classify(url: URL(string: "http://nas.home.arpa:8440")!), .lan)
        XCTAssertEqual(HubRouteKind.classify(url: URL(string: "http://nas.internal:8440")!), .lan)
        XCTAssertEqual(HubRouteKind.classify(url: URL(string: "http://localhome.example:8440")!), .publicNetwork)
    }

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

        // 3. Incompatible API version surfaces `.incompatibleAPI`; non-Steve service surfaces `.notSteveHub`
        let resolverFutureAPI = EndpointResolver { _ in
            HubHelloResponse(name: "steve", version: "9.0.0", api: 99, login: .init(token: true, oidc: false))
        }
        do {
            _ = try await resolverFutureAPI.resolve(publicURL: nil, lanURLs: [lan])
            XCTFail("Expected incompatibleAPI error")
        } catch let err as EndpointResolutionError {
            XCTAssertEqual(err, .incompatibleAPI(version: 99))
        }

        let resolverNotSteve = EndpointResolver { _ in
            HubHelloResponse(name: "nginx", version: "1.0.0", api: 1, login: .init(token: false, oidc: false))
        }
        do {
            _ = try await resolverNotSteve.resolve(publicURL: nil, lanURLs: [lan])
            XCTFail("Expected notSteveHub error")
        } catch let err as EndpointResolutionError {
            XCTAssertEqual(err, .notSteveHub(url: lan))
        }

        // 4. Web-only port serving `/hub.json` resolves to the Hub API port behind it
        let webPortURL = URL(string: "http://192.168.1.20:18511")!
        let behind = await EndpointResolver.resolveHubBehind(webURL: webPortURL) { url in
            XCTAssertEqual(url.absoluteString, "http://192.168.1.20:18511/hub.json")
            let resp = HTTPURLResponse(url: url, statusCode: 200, httpVersion: "HTTP/1.1", headerFields: nil)!
            return (Data("{\"hub\":\"http://192.168.1.20:18501/\"}".utf8), resp)
        }
        XCTAssertEqual(behind?.absoluteString, "http://192.168.1.20:18501")
    }

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

        let envOverride = LocalHubDiscovery.defaultProfile(environment: ["STEVE_PORT": "18545"], steveHome: tempHome)
        XCTAssertEqual(envOverride.orderedEndpoints.map(\.absoluteString), ["http://127.0.0.1:18545"])

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
    func testHubStoreDefaultsToLocalHubAndTracksCookieCleanupOnEndpointChange() {
        let suiteName = "test.steve.hubstore.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let creds = InMemoryCredentialStore()
        let store1 = HubStore(
            defaults: defaults,
            credentials: creds,
            defaultProfile: LocalHubDiscovery.defaultProfile(environment: ["STEVE_PORT": "18540"]),
            includeLocalDefault: true
        )
        store1.updateAuthMetadata(id: HubProfile.localDefaultID, clientID: "c1", userName: "Owner", credential: "cred_18540")
        XCTAssertTrue(store1.drainCookieCleanupHubIDs().isEmpty)

        // Relaunch with STEVE_PORT changed to 18541 -> clears stale credential AND marks `local-default` for cookie purge
        let store2 = HubStore(
            defaults: defaults,
            credentials: creds,
            defaultProfile: LocalHubDiscovery.defaultProfile(environment: ["STEVE_PORT": "18541"]),
            includeLocalDefault: true
        )
        XCTAssertNil(store2.credential(for: HubProfile.localDefaultID))
        XCTAssertEqual(store2.drainCookieCleanupHubIDs(), [HubProfile.localDefaultID])
        XCTAssertTrue(store2.drainCookieCleanupHubIDs().isEmpty)

        let added = store2.addManualHub(address: "https://remote.example.com/", name: "Remote Hub", makeActive: true)!
        XCTAssertEqual(store2.activeProfile.name, "Remote Hub")

        // Updating `added` by ID with a new endpoint marks it for cookie cleanup
        var changed = added
        changed.publicURL = URL(string: "https://remote-v2.example.com")!
        _ = store2.upsertProfile(changed, makeActive: true)
        XCTAssertEqual(store2.drainCookieCleanupHubIDs(), [added.id])

        // Removing `added` marks it for cookie cleanup
        let removed = store2.removeHub(id: added.id)
        XCTAssertEqual(removed?.wasActive, true)
        XCTAssertEqual(store2.drainCookieCleanupHubIDs(), [added.id])

        let purgedPrefix = HubLoopbackProxy.hubCookiePrefix(for: added.id)
        let keptPrefix = HubLoopbackProxy.hubCookiePrefix(for: HubProfile.localDefaultID)
        XCTAssertTrue(HubLoopbackProxy.shouldDeleteWebCookie(named: "\(purgedPrefix)steve_session_8440", purgedHubIDs: [added.id]))
        XCTAssertFalse(HubLoopbackProxy.shouldDeleteWebCookie(named: "\(keptPrefix)steve_session_18541", purgedHubIDs: [added.id]))
        XCTAssertFalse(HubLoopbackProxy.shouldDeleteWebCookie(named: HubLoopbackProxy.proxyAuthCookieName, purgedHubIDs: [added.id]))
    }

    // MARK: - Loopback Proxy Security, Opaque Cookie Isolation & Clear-Site-Data Stripping

    func testLoopbackProxyEnforcesHostOriginCookieSecretAndStripsClearSiteData() async throws {
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

        // 3. Local process hitting `/api/me` WITHOUT `steve_proxy_key` cookie gets 403 Forbidden
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
        //    injects `Authorization: Bearer`, and strips `steve_proxy_key` from upstream `Cookie`.
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

        // 6. Upstream `Clear-Site-Data` header is stripped so a remote Hub cannot wipe `steve_proxy_key` or other Hubs' storage.
        let (_, clearResp) = try await RawTCPHTTPClient.request(
            port: proxy.port,
            path: "/auth/clear-site-data-test",
            headers: [
                ("Host", "127.0.0.1:\(proxy.port)"),
                ("Cookie", proxy.authCookieHeader),
            ]
        )
        XCTAssertEqual(clearResp.status, 200)
        XCTAssertNil(clearResp.header("clear-site-data"), "Proxy must strip Clear-Site-Data from upstream responses")

        // 7. When `baseURL` is nil (no Hub or offline Hub), `/auth/hello` returns immediate 503 JSON while `/` still serves WebDist.
        proxy.updateTarget(.init(hubID: "hub-main", baseURL: nil, credential: nil))
        let (_, offlineHelloResp) = try await RawTCPHTTPClient.request(
            port: proxy.port,
            path: "/auth/hello",
            headers: [
                ("Host", "127.0.0.1:\(proxy.port)"),
                ("Cookie", proxy.authCookieHeader),
            ]
        )
        XCTAssertEqual(offlineHelloResp.status, 503)
        let (_, offlineIndexResp) = try await RawTCPHTTPClient.request(
            port: proxy.port,
            path: "/",
            headers: [("Host", "127.0.0.1:\(proxy.port)")]
        )
        XCTAssertEqual(offlineIndexResp.status, 200)
    }

    func testLoopbackProxyIsolatesOpaquePortCookiesAcrossHubSwitches() async throws {
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
        XCTAssertTrue(setCookies.contains { $0.hasPrefix("\(alphaPrefix)steve_session_18521=sess_cred_a;") && !$0.lowercased().contains("secure") })
        XCTAssertTrue(setCookies.contains { $0.hasPrefix("\(alphaPrefix)steve_ws_18521=workspace_a;") && $0.contains("SameSite=Lax") })

        let browserCookieJar = [
            proxy.authCookieHeader,
            "\(alphaPrefix)steve_session_18521=sess_cred_a",
            "\(alphaPrefix)steve_ws_18521=workspace_a",
            "unscoped_cookie=foreign",
        ].joined(separator: "; ")

        let (echoAData, _) = try await RawTCPHTTPClient.request(
            port: proxy.port,
            path: "/api/me",
            headers: [
                ("Host", "127.0.0.1:\(proxy.port)"),
                ("Cookie", browserCookieJar),
            ]
        )
        let echoA = try JSONSerialization.jsonObject(with: echoAData) as? [String: String]
        XCTAssertEqual(echoA?["cookie"], "steve_session_18521=sess_cred_a; steve_ws_18521=workspace_a")

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
        XCTAssertEqual(url2, url1)

        let proxy3 = HubLoopbackProxy(preferredPortDefaults: defaults)
        let url3 = try await proxy3.start()
        XCTAssertNotEqual(url3, url1)
        XCTAssertGreaterThan(proxy3.port, 0)
        proxy2.stop()
        proxy3.stop()
    }

    // MARK: - Native Bridge v2 Operations & Confirmation Gates

    @MainActor
    func testNativeBridgeV2OperationsListAddSwitchForgetPairClaimAndScan() async throws {
        let mockHubA = try await MockUpstreamServer.start()
        defer { mockHubA.stop() }
        let mockHubB = try await MockUpstreamServer.start()
        defer { mockHubB.stop() }

        let suiteName = "test.steve.bridgev2.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        var purgedSets: [Set<String>] = []
        let creds = InMemoryCredentialStore()
        let store = HubStore(
            defaults: defaults,
            credentials: creds,
            defaultProfile: HubProfile(id: HubProfile.localDefaultID, name: "Local Hub", lanURLs: [mockHubA.baseURL], isLocalDefault: true),
            includeLocalDefault: true
        )
        let coordinator = SteveClientCoordinator(
            hubStore: store,
            preferredPortDefaults: defaults,
            clientDescriptor: ClientDescriptor(kind: "mac", name: "Test Mac", platform: "macOS", version: "0.3.0"),
            purgeWebCookiesHandler: { ids in purgedSets.append(ids) }
        )
        defer { coordinator.stop() }

        XCTAssertEqual(
            SteveClientCoordinator.bridgeCapabilities(for: "macos"),
            ["hubs.list", "hubs.switch", "hubs.add", "hubs.forget", "pair.claim"]
        )
        XCTAssertEqual(
            SteveClientCoordinator.bridgeCapabilities(for: "ios"),
            ["hubs.list", "hubs.switch", "hubs.add", "hubs.forget", "pair.claim", "pair.scan"]
        )

        await coordinator.activateCurrentHub(forceReloadWeb: false)

        // 1. `hubs.list`
        let listRes1 = try await coordinator.handleBridgeCall(op: "hubs.list", args: [:], platform: "macos")
        let hubs1 = try XCTUnwrap(listRes1["hubs"] as? [[String: Any]])
        XCTAssertEqual(hubs1.count, 1)
        XCTAssertEqual(hubs1[0]["id"] as? String, HubProfile.localDefaultID)
        XCTAssertEqual(hubs1[0]["current"] as? Bool, true)
        XCTAssertEqual(hubs1[0]["removable"] as? Bool, false)
        XCTAssertEqual(hubs1[0]["route"] as? String, "loopback")

        // 2. `hubs.add` probes before saving: dead port rejects without adding to HubStore
        do {
            _ = try await coordinator.handleBridgeCall(
                op: "hubs.add",
                args: ["address": "http://127.0.0.1:9", "name": "Dead Hub"],
                platform: "macos"
            )
            XCTFail("Unreachable Hub must fail hubs.add before persisting")
        } catch {
            XCTAssertEqual(store.profiles.count, 1)
        }

        // Valid `hubs.add` saves and activates Hub B
        _ = try await coordinator.handleBridgeCall(
            op: "hubs.add",
            args: ["address": mockHubB.baseURL.absoluteString, "name": "Lab Hub"],
            platform: "macos"
        )
        XCTAssertEqual(store.profiles.count, 2)
        XCTAssertEqual(store.activeProfile.name, "Lab Hub")

        // 3. `hubs.switch` back to Local Hub
        _ = try await coordinator.handleBridgeCall(
            op: "hubs.switch",
            args: ["id": HubProfile.localDefaultID],
            platform: "macos"
        )
        XCTAssertEqual(store.activeHubID, HubProfile.localDefaultID)

        // 4. `hubs.forget` refuses `local-default`, removes `Lab Hub`, and triggers cookie purge
        do {
            _ = try await coordinator.handleBridgeCall(
                op: "hubs.forget",
                args: ["id": HubProfile.localDefaultID],
                platform: "macos"
            )
            XCTFail("local-default must not be removable")
        } catch {}

        let labID = try XCTUnwrap(store.profiles.first(where: { !$0.isLocalDefault })?.id)
        _ = try await coordinator.handleBridgeCall(op: "hubs.forget", args: ["id": labID], platform: "macos")
        XCTAssertEqual(store.profiles.count, 1)
        XCTAssertTrue(purgedSets.contains([labID]))

        // 5. `pair.claim` with HTTP `#/pair?...` link stages confirmation; Cancel resolves cleanly without claiming
        let lanEncoded = mockHubB.baseURL.absoluteString.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)!
        let httpPairLink = "http://127.0.0.1:59999/#/pair?v=1&code=stp_0123456789abcdef0123456789abcdef&name=Paired+Lab&lan=\(lanEncoded)"

        let cancelClaimTask = Task {
            try await coordinator.handleBridgeCall(op: "pair.claim", args: ["link": httpPairLink], platform: "macos")
        }
        try await Task.sleep(nanoseconds: 40_000_000)
        XCTAssertNotNil(coordinator.pendingExternalPairConfirmation)
        XCTAssertEqual(mockHubB.pairCallCount(), 0)
        coordinator.cancelPendingExternalPairing()
        _ = try await cancelClaimTask.value
        XCTAssertEqual(mockHubB.pairCallCount(), 0)
        XCTAssertEqual(store.profiles.count, 1)

        // Confirming `pair.claim` claims code, saves Keychain credential, and switches active Hub
        let confirmClaimTask = Task {
            try await coordinator.handleBridgeCall(op: "pair.claim", args: ["link": httpPairLink], platform: "macos")
        }
        try await Task.sleep(nanoseconds: 40_000_000)
        XCTAssertNotNil(coordinator.pendingExternalPairConfirmation)
        _ = await coordinator.confirmPendingExternalPairing()
        _ = try await confirmClaimTask.value
        XCTAssertEqual(mockHubB.pairCallCount(), 1)
        XCTAssertEqual(store.activeProfile.name, "Paired Lab")
        XCTAssertEqual(store.credential(for: store.activeHubID), "cli_paired.secret_cred")

        // 6. `pair.scan` rejected on macOS, accepted on iOS
        do {
            _ = try await coordinator.handleBridgeCall(op: "pair.scan", args: [:], platform: "macos")
            XCTFail("pair.scan must not succeed on macOS")
        } catch {}
        _ = try await coordinator.handleBridgeCall(op: "pair.scan", args: [:], platform: "ios")
        XCTAssertTrue(coordinator.isQRScannerPresented)
    }

    // MARK: - N3 HttpOnly Proxy Cookie & WKContentRuleList Cross-Port Loopback Blocking

    @MainActor
    func testHttpOnlyProxyCookieAndWKContentRuleListBlocksOtherLoopbackPortsInWebView() async throws {
        let tempDir = FileManager.default.temporaryDirectory.appendingPathComponent("steve-wv-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tempDir) }
        try Data("<!doctype html><html><body><div id='app'>ready</div></body></html>".utf8)
            .write(to: tempDir.appendingPathComponent("index.html"))

        let mockHub = try await MockUpstreamServer.start()
        defer { mockHub.stop() }
        let foreignLocalServer = try await MockUpstreamServer.start()
        defer { foreignLocalServer.stop() }

        let suiteName = "test.steve.webview-isolation.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let store = HubStore(
            defaults: defaults,
            credentials: InMemoryCredentialStore(),
            defaultProfile: HubProfile(id: HubProfile.localDefaultID, name: "Local Hub", lanURLs: [mockHub.baseURL], isLocalDefault: true),
            includeLocalDefault: true
        )
        let coordinator = SteveClientCoordinator(
            hubStore: store,
            webRoot: tempDir,
            preferredPortDefaults: defaults,
            clientDescriptor: ClientDescriptor(kind: "mac", name: "Test Mac", platform: "macOS", version: "0.3.0")
        )
        defer { coordinator.stop() }

        let gateway = try await coordinator.proxy.ensureRunning()
        await coordinator.activateCurrentHub(forceReloadWeb: false)

        // 1. Verify `makeProxyAuthCookie` produces a true `HttpOnly` cookie
        let cookie = try XCTUnwrap(SteveWebView.makeProxyAuthCookie(gateway: gateway, token: coordinator.proxy.proxyAuthToken))
        XCTAssertTrue(cookie.isHTTPOnly, "steve_proxy_key must be HttpOnly so document.cookie cannot read it")
        XCTAssertEqual(cookie.name, HubLoopbackProxy.proxyAuthCookieName)

        // 2. Compile `WKContentRuleList` and load real `WKWebView` at gateway origin
        let ruleList = try await SteveWebView.compileLoopbackPortRuleList(gatewayPort: Int(coordinator.proxy.port))
        let bridgeView = SteveWebView(coordinator: coordinator, platformName: "macos")
        let bridge = bridgeView.makeCoordinator()

        let config = WKWebViewConfiguration()
        config.websiteDataStore = .nonPersistent()
        let uc = WKUserContentController()
        uc.addScriptMessageHandler(bridge, contentWorld: .page, name: SteveWebView.BridgeCoordinator.handlerName)
        uc.addUserScript(
            WKUserScript(
                source: SteveWebView.makeBootstrapScript(platform: "macos", gateway: gateway),
                injectionTime: .atDocumentStart,
                forMainFrameOnly: true
            )
        )
        uc.add(ruleList)
        config.userContentController = uc

        let webView = WKWebView(frame: CGRect(x: 0, y: 0, width: 400, height: 300), configuration: config)
        webView.navigationDelegate = bridge
        bridge.loadedGateway = gateway

        await withCheckedContinuation { (cont: CheckedContinuation<Void, Never>) in
            config.websiteDataStore.httpCookieStore.setCookie(cookie) { cont.resume() }
        }
        webView.load(URLRequest(url: gateway))

        for _ in 0..<50 {
            if bridge.isPageLoaded { break }
            try await Task.sleep(nanoseconds: 50_000_000)
        }
        XCTAssertTrue(bridge.isPageLoaded, "WKWebView should finish loading bundled index.html from gateway")

        // 3. `document.cookie` must NOT expose `steve_proxy_key`
        let docCookie = try await webView.evaluateJavaScript("document.cookie") as? String ?? ""
        XCTAssertFalse(docCookie.contains(HubLoopbackProxy.proxyAuthCookieName))

        // 4. Same-gateway fetch `/auth/hello` succeeds AND `window.__STEVE_NATIVE__.call('hubs.list')` works
        let bridgeCheck = try await webView.callAsyncJavaScript(
            """
            const helloResp = await fetch('/auth/hello');
            const hello = await helloResp.json();
            const hubsRes = await window.__STEVE_NATIVE__.call('hubs.list', {});
            return {
              v: window.__STEVE_NATIVE__.v,
              platform: window.__STEVE_NATIVE__.platform,
              helloName: hello.name,
              hubCount: hubsRes.hubs.length
            };
            """,
            arguments: [:],
            in: nil,
            contentWorld: .page
        ) as? [String: Any]
        XCTAssertEqual(bridgeCheck?["v"] as? Int, 2)
        XCTAssertEqual(bridgeCheck?["platform"] as? String, "macos")
        XCTAssertEqual(bridgeCheck?["helloName"] as? String, "steve")
        XCTAssertEqual(bridgeCheck?["hubCount"] as? Int, 1)

        // 5. Cross-port fetch to `http://127.0.0.1:<foreignPort>/auth/hello` is blocked by `WKContentRuleList`
        let crossPortBlocked = try await webView.callAsyncJavaScript(
            """
            try {
              await fetch(targetURL);
              return false;
            } catch (_) {
              return true;
            }
            """,
            arguments: ["targetURL": foreignLocalServer.baseURL.appendingPathComponent("auth/hello").absoluteString],
            in: nil,
            contentWorld: .page
        ) as? Bool
        XCTAssertEqual(crossPortBlocked, true, "WKContentRuleList must block fetches to any other port on 127.0.0.1")
        XCTAssertEqual(foreignLocalServer.requestCount(), 0, "Blocked cross-port loopback request must never hit the foreign socket")
    }

    // MARK: - Stale Credential Poll Race & Retired Credential Revocation

    @MainActor
    func testBackgroundPollCannotOverwriteNewSignInCredentialAndReplacedCredentialIsRetired() async throws {
        let mockHub = try await MockUpstreamServer.start()
        defer { mockHub.stop() }

        let suiteName = "test.steve.credrace.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let hello = HubHelloResponse(name: "steve", version: "0.3.0", api: 1, login: .init(token: true, oidc: false))
        let slowResolver = EndpointResolver { _ in
            try await Task.sleep(nanoseconds: 250_000_000)
            return hello
        }

        let creds = InMemoryCredentialStore()
        let store = HubStore(
            defaults: defaults,
            credentials: creds,
            defaultProfile: HubProfile(id: HubProfile.localDefaultID, name: "Local Hub", lanURLs: [mockHub.baseURL], isLocalDefault: true),
            includeLocalDefault: true
        )
        let coordinator = SteveClientCoordinator(
            hubStore: store,
            resolver: slowResolver,
            preferredPortDefaults: defaults,
            clientDescriptor: ClientDescriptor(kind: "mac", name: "Test Mac", platform: "macOS", version: "0.3.0")
        )
        defer { coordinator.stop() }

        let gateway = try await coordinator.proxy.ensureRunning()
        coordinator.proxy.updateTarget(.init(hubID: HubProfile.localDefaultID, baseURL: mockHub.baseURL, credential: nil))

        // Start a slow background poll while credential is nil, then sign in via `/auth/token` BEFORE poll finishes
        let inFlightPoll = Task {
            await coordinator.activateCurrentHub(forceReloadWeb: false)
        }
        try await Task.sleep(nanoseconds: 40_000_000)

        let (_, login1) = try await RawTCPHTTPClient.request(
            port: coordinator.proxy.port,
            method: "POST",
            path: "/auth/token",
            headers: [
                ("Host", "127.0.0.1:\(coordinator.proxy.port)"),
                ("Origin", gateway.absoluteString),
                ("Cookie", coordinator.proxy.authCookieHeader),
            ]
        )
        XCTAssertEqual(login1.status, 200)

        await inFlightPoll.value
        try await Task.sleep(nanoseconds: 100_000_000)

        // The stale poll must NOT clobber the newly stored credential back to nil!
        XCTAssertEqual(store.credential(for: HubProfile.localDefaultID), "cli_web_tok.secret_cred")
        XCTAssertEqual(coordinator.proxy.currentTarget().credential, "cli_web_tok.secret_cred")

        // Now seed an older credential and sign in again: the replaced credential must be retired on `/auth/client/logout`
        store.updateAuthMetadata(id: HubProfile.localDefaultID, clientID: "cli_old", userName: "Owner", credential: "cli_old.stale_secret")
        let (_, login2) = try await RawTCPHTTPClient.request(
            port: coordinator.proxy.port,
            method: "POST",
            path: "/auth/token",
            headers: [
                ("Host", "127.0.0.1:\(coordinator.proxy.port)"),
                ("Origin", gateway.absoluteString),
                ("Cookie", coordinator.proxy.authCookieHeader),
            ]
        )
        XCTAssertEqual(login2.status, 200)
        try await Task.sleep(nanoseconds: 150_000_000)
        XCTAssertEqual(mockHub.lastLogoutAuth(), "Bearer cli_old.stale_secret")
        XCTAssertEqual(store.credential(for: HubProfile.localDefaultID), "cli_web_tok.secret_cred")
    }

    // MARK: - Pairing Confirmation Isolation, Subframe Confinement & Downloads (N5)

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

        let lanLocalEncoded = "http://127.0.0.1:18540".addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)!
        let attackerPubEncoded = attackerNoCredHub.baseURL.absoluteString.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)!
        let evilURI = "steve://pair?v=1&code=stp_0123456789abcdef0123456789abcdef&name=EvilHub&pub=\(attackerPubEncoded)&lan=\(lanLocalEncoded)"

        coordinator.requestExternalPairingConfirmation(uri: evilURI)
        XCTAssertNotNil(coordinator.pendingExternalPairConfirmation)
        XCTAssertEqual(store.profiles.count, 1)

        await coordinator.confirmPendingExternalPairing()
        XCTAssertNotNil(coordinator.externalPairErrorMessage)
        XCTAssertEqual(store.profiles.count, 1)
        XCTAssertEqual(store.credential(for: HubProfile.localDefaultID), "cli_local_owner.top_secret")

        let validPubEncoded = realNewHub.baseURL.absoluteString.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)!
        let overlappingURI = "steve://pair?v=1&code=stp_fedcba9876543210fedcba9876543210&name=PairedHub&pub=\(validPubEncoded)&lan=\(lanLocalEncoded)"
        coordinator.requestExternalPairingConfirmation(uri: overlappingURI)
        let confirmedProfile = await coordinator.confirmPendingExternalPairing()
        let pairedProfile = try XCTUnwrap(confirmedProfile)
        XCTAssertNotEqual(pairedProfile.id, HubProfile.localDefaultID)
        XCTAssertEqual(store.profiles.count, 2)
        XCTAssertEqual(store.credential(for: HubProfile.localDefaultID), "cli_local_owner.top_secret")
        XCTAssertEqual(store.credential(for: pairedProfile.id), "cli_paired.secret_cred")
    }

    @MainActor
    func testScannerStagingRequiresConfirmationAndSanitizesPairingDisplayName() async throws {
        let mockHub = try await MockUpstreamServer.start()
        defer { mockHub.stop() }

        let suiteName = "test.steve.scanner-confirm.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let store = HubStore(
            defaults: defaults,
            credentials: InMemoryCredentialStore(),
            includeLocalDefault: false
        )
        let coordinator = SteveClientCoordinator(
            hubStore: store,
            preferredPortDefaults: defaults,
            clientDescriptor: ClientDescriptor(kind: "ios", name: "Test iPhone", platform: "iOS", version: "0.3.0")
        )
        defer { coordinator.stop() }

        let spoofedName = "Trusted Mac\r\n\u{202E}Target: https://trusted.local\u{0007} " + String(repeating: "X", count: 80)
        var allowed = CharacterSet.urlQueryAllowed
        allowed.remove(charactersIn: "&=+")
        let encodedName = spoofedName.addingPercentEncoding(withAllowedCharacters: allowed)!
        let lanEncoded = mockHub.baseURL.absoluteString.addingPercentEncoding(withAllowedCharacters: allowed)!
        let uri = "https://hub.example.com/#/pair?v=1&code=stp_11223344556677889900aabbccddeeff&name=\(encodedName)&lan=\(lanEncoded)"

        coordinator.isQRScannerPresented = true
        XCTAssertTrue(coordinator.stageScannedPairingCode(uri))
        XCTAssertFalse(coordinator.isQRScannerPresented)

        let pending = try XCTUnwrap(coordinator.pendingExternalPairConfirmation)
        XCTAssertFalse(pending.displayName.contains("\n"))
        XCTAssertFalse(pending.displayName.contains("\r"))
        XCTAssertFalse(pending.displayName.unicodeScalars.contains(where: { $0.value == 0x202E || $0.value == 0x0007 }))
        XCTAssertLessThanOrEqual(pending.displayName.count, PairingPayload.maxNameLength)
        XCTAssertEqual(mockHub.pairCallCount(), 0)

        coordinator.cancelPendingExternalPairing()
        XCTAssertNil(coordinator.pendingExternalPairConfirmation)
        XCTAssertEqual(mockHub.pairCallCount(), 0)
    }

    func testNavigationPolicyDownloadsAndFilenameDeduplication() throws {
        let gateway = URL(string: "http://127.0.0.1:51234")!
        let activeHub = URL(string: "https://hub.example.com:8443")!
        let external = URL(string: "https://github.com/org/repo/releases")!

        // 1. `<a download>` on `blob:` or gateway URL triggers `.download`
        XCTAssertEqual(
            SteveWebView.decideNavigationDisposition(
                url: URL(string: "blob:http://127.0.0.1:51234/abc")!,
                gateway: gateway,
                activeHubEndpoints: [activeHub],
                shouldPerformDownload: true,
                targetFrameIsMain: true,
                sourceFrameIsMain: true,
                navigationType: .linkActivated
            ),
            .download
        )

        // 2. `target="_blank"` on gateway `/api/files/...` or active Hub `/api/files/...` routes to authenticated gateway download!
        XCTAssertEqual(
            SteveWebView.decideNavigationDisposition(
                url: URL(string: "https://hub.example.com:8443/api/files/f_123?download=1")!,
                gateway: gateway,
                activeHubEndpoints: [activeHub],
                targetFrameIsMain: nil,
                sourceFrameIsMain: true,
                navigationType: .linkActivated
            ),
            .startGatewayDownload(URL(string: "http://127.0.0.1:51234/api/files/f_123?download=1")!)
        )

        // 3. External `target="_blank"` non-Hub URL still opens in system browser; scripted subframe external is cancelled
        XCTAssertEqual(
            SteveWebView.decideNavigationDisposition(
                url: external,
                gateway: gateway,
                activeHubEndpoints: [activeHub],
                targetFrameIsMain: nil,
                sourceFrameIsMain: true,
                navigationType: .linkActivated
            ),
            .openExternal(external)
        )
        XCTAssertEqual(
            SteveWebView.decideNavigationDisposition(
                url: external,
                gateway: gateway,
                activeHubEndpoints: [activeHub],
                targetFrameIsMain: false,
                sourceFrameIsMain: false,
                navigationType: .other
            ),
            .cancel
        )

        // 4. `Content-Disposition: attachment` response from gateway triggers download, while 404 JSON does not
        let attachResp = HTTPURLResponse(
            url: URL(string: "http://127.0.0.1:51234/api/tasks/t1/artifacts/a.zip?download=1")!,
            statusCode: 200,
            httpVersion: "HTTP/1.1",
            headerFields: ["Content-Disposition": "attachment; filename=\"a.zip\""]
        )!
        XCTAssertTrue(SteveWebView.shouldDownloadResponse(attachResp, canShowMIMEType: true, gateway: gateway))

        let errResp = HTTPURLResponse(
            url: URL(string: "http://127.0.0.1:51234/api/tasks/t1/artifacts/missing")!,
            statusCode: 404,
            httpVersion: "HTTP/1.1",
            headerFields: ["Content-Disposition": "attachment; filename=\"missing\""]
        )!
        XCTAssertFalse(SteveWebView.shouldDownloadResponse(errResp, canShowMIMEType: false, gateway: gateway))

        // 5. Unique download destination deduplication & filename sanitization
        let dlDir = FileManager.default.temporaryDirectory.appendingPathComponent("steve-dl-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dlDir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dlDir) }

        let first = SteveWebView.uniqueDownloadDestination(in: dlDir, suggestedFilename: "report.pdf")
        XCTAssertEqual(first.lastPathComponent, "report.pdf")
        try Data("1".utf8).write(to: first)

        let second = SteveWebView.uniqueDownloadDestination(in: dlDir, suggestedFilename: "report.pdf")
        XCTAssertEqual(second.lastPathComponent, "report (1).pdf")
        try Data("2".utf8).write(to: second)

        let third = SteveWebView.uniqueDownloadDestination(in: dlDir, suggestedFilename: "report.pdf")
        XCTAssertEqual(third.lastPathComponent, "report (2).pdf")

        XCTAssertEqual(SteveWebView.sanitizeSuggestedFilename("../../etc/passwd"), ".._.._etc_passwd")
        XCTAssertEqual(SteveWebView.sanitizeSuggestedFilename(".."), "download")
    }

    @MainActor
    func testAddPairedProfileThrowsWhenCredentialSaveFails() async throws {
        final class FailingCredentialStore: CredentialStoring, @unchecked Sendable {
            func credential(for hubID: String) -> String? { nil }
            func setCredential(_ credential: String?, for hubID: String) -> Bool { false }
        }

        let mockHub = try await MockUpstreamServer.start()
        defer { mockHub.stop() }

        let suiteName = "test.steve.keychain-fail.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let store = HubStore(
            defaults: defaults,
            credentials: FailingCredentialStore(),
            includeLocalDefault: false
        )
        let coordinator = SteveClientCoordinator(
            hubStore: store,
            preferredPortDefaults: defaults,
            clientDescriptor: ClientDescriptor(kind: "ios", name: "Test iPhone", platform: "iOS", version: "0.3.0")
        )
        defer { coordinator.stop() }

        let lanEncoded = mockHub.baseURL.absoluteString.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)!
        let uri = "steve://pair?v=1&code=stp_aabbccddeeff00112233445566778899&name=FailKeychainHub&lan=\(lanEncoded)"
        XCTAssertTrue(coordinator.stageScannedPairingCode(uri))

        let result = await coordinator.confirmPendingExternalPairing()
        XCTAssertNil(result)
        XCTAssertFalse(store.hasProfiles)
        XCTAssertEqual(coordinator.externalPairErrorMessage, CredentialStoreError.saveFailed.localizedDescription)
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
    private var recordedPairCalls = 0
    private var recordedTotalRequests = 0

    private init(listener: NWListener, port: UInt16) {
        self.listener = listener
        self.baseURL = URL(string: "http://127.0.0.1:\(port)")!
    }

    func lastLogoutAuth() -> String? {
        lock.lock()
        defer { lock.unlock() }
        return recordedLogoutAuth
    }

    func pairCallCount() -> Int {
        lock.lock()
        defer { lock.unlock() }
        return recordedPairCalls
    }

    func requestCount() -> Int {
        lock.lock()
        defer { lock.unlock() }
        return recordedTotalRequests
    }

    private func recordRequest() {
        lock.lock()
        recordedTotalRequests += 1
        lock.unlock()
    }

    private func recordLogout(_ auth: String) {
        lock.lock()
        recordedLogoutAuth = auth
        lock.unlock()
    }

    private func recordPairCall() {
        lock.lock()
        recordedPairCalls += 1
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
                    box.server?.recordRequest()
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

                    if path == "/auth/clear-site-data-test" {
                        let payload = Data("{\"ok\":true}".utf8)
                        let head =
                            "HTTP/1.1 200 OK\r\n"
                            + "Content-Type: application/json\r\n"
                            + "Clear-Site-Data: \"cookies\", \"storage\"\r\n"
                            + "Content-Length: \(payload.count)\r\n"
                            + "Connection: close\r\n\r\n"
                        conn.send(content: Data(head.utf8) + payload, completion: .contentProcessed { _ in conn.cancel() })
                        return
                    }

                    if path == "/auth/set-multi-cookie" {
                        let payload = Data("{\"ok\":true}".utf8)
                        let head =
                            "HTTP/1.1 200 OK\r\n"
                            + "Content-Type: application/json\r\n"
                            + "Set-Cookie: steve_session_18521=sess_cred_a; Path=/; HttpOnly; Secure\r\n"
                            + "Set-Cookie: steve_ws_18521=workspace_a; Expires=Wed, 11 Nov 2026 12:00:00 GMT; Path=/; SameSite=None; Secure\r\n"
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
                        box.server?.recordPairCall()
                        if omitPairCredential {
                            payload = Data("{\"client_id\":\"cli_evil\",\"user\":{\"id\":\"owner\",\"name\":\"Owner\"},\"hub\":{\"version\":\"0.3.0\",\"api\":1}}".utf8)
                        } else {
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
