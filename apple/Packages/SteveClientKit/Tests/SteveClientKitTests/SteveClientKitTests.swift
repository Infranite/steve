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
        XCTAssertEqual(URLNormalizer.normalize("http://[fd00::1]:8440")?.absoluteString, "http://[fd00::1]:8440")
        XCTAssertEqual(URLNormalizer.normalize("http://[FD00::1]:8440/")?.absoluteString, "http://[fd00::1]:8440")
        XCTAssertEqual(URLNormalizer.normalize("[fd00::1]:8440")?.absoluteString, "http://[fd00::1]:8440")
        XCTAssertEqual(URLNormalizer.normalize("http://[::1]:80/")?.absoluteString, "http://[::1]")
        XCTAssertNil(URLNormalizer.normalize("ftp://steve.example.com"))
        XCTAssertNil(URLNormalizer.normalize("steve://pair?code=stp_0123456789abcdef"))
        XCTAssertNil(URLNormalizer.normalize("javascript:alert(1)"))
        XCTAssertNil(URLNormalizer.normalize("http://user:pass@192.168.1.10:8440"))

        // RFC 6598 100.64.0.0/10 -> vendor-neutral `.privateOverlay` ("overlay"), bridge wire code `"tailnet"`
        XCTAssertEqual(HubRouteKind.classify(url: URL(string: "http://100.64.0.1:8440")!), .privateOverlay)
        XCTAssertEqual(HubRouteKind.classify(url: URL(string: "http://100.127.255.254:8440")!), .privateOverlay)
        XCTAssertEqual(HubRouteKind.privateOverlay.rawValue, "overlay")
        XCTAssertEqual(HubRouteKind.privateOverlay.bridgeRouteValue, "tailnet")
        XCTAssertEqual(HubRouteKind.loopback.bridgeRouteValue, "loopback")
        XCTAssertEqual(HubRouteKind.lan.bridgeRouteValue, "lan")
        XCTAssertEqual(HubRouteKind.proxy.bridgeRouteValue, "proxy")
        XCTAssertEqual(HubRouteKind.publicNetwork.bridgeRouteValue, "public")
        XCTAssertNil(HubRouteKind.unknown.bridgeRouteValue)
        XCTAssertEqual(HubRouteKind.classify(url: URL(string: "http://100.63.255.255:8440")!), .publicNetwork)
        XCTAssertEqual(HubRouteKind.classify(url: URL(string: "http://100.128.0.1:8440")!), .publicNetwork)
        XCTAssertEqual(HubRouteKind.classify(url: URL(string: "https://node.example.ts.net")!), .publicNetwork)
        XCTAssertEqual(HubRouteKind.classify(url: URL(string: "http://[fd00::1]:8440")!), .lan)
        XCTAssertEqual(HubRouteKind.classify(url: URL(string: "http://[fe80::1]:8440")!), .lan)
        XCTAssertEqual(HubRouteKind.classify(url: URL(string: "http://[::1]:8440")!), .loopback)

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
        let pendingCancel = try await waitForPendingPairConfirmation(on: coordinator)
        XCTAssertEqual(mockHubB.pairCallCount(), 0)
        coordinator.cancelPendingExternalPairing(id: pendingCancel.id)
        let cancelResult = try await awaitWithTimeout(seconds: 5.0) { try await cancelClaimTask.value }
        XCTAssertTrue(cancelResult.isEmpty)
        XCTAssertEqual(mockHubB.pairCallCount(), 0)
        XCTAssertEqual(store.profiles.count, 1)

        // Confirming `pair.claim`: the alert's setter hides it, the Pair button takes it by id. That claims the
        // code, saves the Keychain credential, and switches the active Hub.
        let confirmClaimTask = Task {
            try await coordinator.handleBridgeCall(op: "pair.claim", args: ["link": httpPairLink], platform: "macos")
        }
        let pendingConfirm = try await waitForPendingPairConfirmation(on: coordinator)
        coordinator.hidePairConfirmationAlert()
        coordinator.beginConfirmExternalPairing(id: pendingConfirm.id)
        _ = try await awaitWithTimeout(seconds: 5.0) { try await confirmClaimTask.value }
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

    @MainActor
    func testConcurrentPairingRequestsAreRefusedAndConfirmByIDClaimsOnlyFirstHub() async throws {
        let mockHubX = try await MockUpstreamServer.start()
        defer { mockHubX.stop() }
        let mockHubY = try await MockUpstreamServer.start()
        defer { mockHubY.stop() }

        let suiteName = "test.steve.pair-concurrent.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let store = HubStore(
            defaults: defaults,
            credentials: InMemoryCredentialStore(),
            defaultProfile: HubProfile(id: HubProfile.localDefaultID, name: "Local Hub", lanURLs: [URL(string: "http://127.0.0.1:9")!], isLocalDefault: true),
            includeLocalDefault: true
        )
        let coordinator = SteveClientCoordinator(
            hubStore: store,
            preferredPortDefaults: defaults,
            clientDescriptor: ClientDescriptor(kind: "mac", name: "Test Mac", platform: "macOS", version: "0.3.0")
        )
        defer { coordinator.stop() }

        let lanX = mockHubX.baseURL.absoluteString.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)!
        let lanY = mockHubY.baseURL.absoluteString.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)!
        let linkX = "steve://pair?v=1&code=stp_11111111111111111111111111111111&name=Hub+X&lan=\(lanX)"
        let linkY = "steve://pair?v=1&code=stp_22222222222222222222222222222222&name=Hub+Y&lan=\(lanY)"

        let firstClaimTask = Task {
            try await coordinator.handleBridgeCall(op: "pair.claim", args: ["link": linkX], platform: "macos")
        }
        let firstPending = try await waitForPendingPairConfirmation(on: coordinator)
        XCTAssertEqual(firstPending.displayName, "Hub X")

        // Second `pair.claim` while first confirmation is on screen MUST reject with an error and NOT replace Hub X
        do {
            _ = try await coordinator.handleBridgeCall(op: "pair.claim", args: ["link": linkY], platform: "macos")
            XCTFail("Concurrent pair.claim must be refused while confirmation is active")
        } catch {}

        // Concurrent `steve://pair` deep link and scanner staging must also be refused without replacing Hub X
        // (the confirmation on screen is the answer: no error is stored to show up after it)
        coordinator.requestExternalPairingConfirmation(uri: linkY)
        XCTAssertNil(coordinator.externalPairErrorMessage)
        XCTAssertFalse(coordinator.stageScannedPairingCode(linkY))
        XCTAssertNil(coordinator.externalPairErrorMessage)
        XCTAssertNotNil(coordinator.pairingRefusalMessage)
        XCTAssertEqual(coordinator.pendingExternalPairConfirmation?.id, firstPending.id)
        XCTAssertEqual(coordinator.pendingExternalPairConfirmation?.displayName, "Hub X")

        // Confirming with an unknown UUID does nothing; confirming `firstPending.id` claims ONLY Hub X
        let wrongAttempt = await coordinator.confirmPendingExternalPairing(id: UUID())
        XCTAssertNil(wrongAttempt)
        XCTAssertEqual(mockHubX.pairCallCount(), 0)

        let claimed = await coordinator.confirmPendingExternalPairing(id: firstPending.id)
        XCTAssertEqual(claimed?.name, "Hub X")
        _ = try await awaitWithTimeout(seconds: 5.0) { try await firstClaimTask.value }
        XCTAssertEqual(mockHubX.pairCallCount(), 1)
        XCTAssertEqual(mockHubY.pairCallCount(), 0)
        XCTAssertEqual(store.activeProfile.name, "Hub X")

        // Non-cancel termination (`coordinator.stop()`) must reject `pair.claim`, never resolve `{}`
        let stoppedClaimTask = Task {
            try await coordinator.handleBridgeCall(op: "pair.claim", args: ["link": linkY], platform: "macos")
        }
        _ = try await waitForPendingPairConfirmation(on: coordinator)
        coordinator.stop()
        do {
            _ = try await awaitWithTimeout(seconds: 5.0) { try await stoppedClaimTask.value }
            XCTFail("Stopping coordinator during pending pair.claim must reject, not resolve {}")
        } catch {}
        XCTAssertEqual(mockHubY.pairCallCount(), 0)
    }

    @MainActor
    private func makePairingGateCoordinator(
        suite: String
    ) -> (coordinator: SteveClientCoordinator, store: HubStore, cleanup: () -> Void) {
        let suiteName = "test.steve.\(suite).\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        let store = HubStore(
            defaults: defaults,
            credentials: InMemoryCredentialStore(),
            defaultProfile: HubProfile(id: HubProfile.localDefaultID, name: "Local Hub", lanURLs: [URL(string: "http://127.0.0.1:9")!], isLocalDefault: true),
            includeLocalDefault: true
        )
        let coordinator = SteveClientCoordinator(
            hubStore: store,
            preferredPortDefaults: defaults,
            clientDescriptor: ClientDescriptor(kind: "mac", name: "Test Mac", platform: "macOS", version: "0.3.0")
        )
        return (coordinator, store, {
            coordinator.stop()
            defaults.removePersistentDomain(forName: suiteName)
        })
    }

    private func pairLink(code: Character, name: String, hub: MockUpstreamServer) -> String {
        let lan = hub.baseURL.absoluteString.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)!
        return "steve://pair?v=1&code=stp_\(String(repeating: code, count: 32))&name=\(name)&lan=\(lan)"
    }

    /// Lets every task already scheduled on the main actor run, so a cancel that the setter had
    /// scheduled (as it used to) would have run by the time this returns.
    @MainActor
    private func drainMainActor() async {
        for _ in 0..<50 {
            await Task.yield()
        }
    }

    @MainActor
    func testPairAlertSetterOnlyHidesAndPairByIDStillClaimsInEitherOrder() async throws {
        let hub = try await MockUpstreamServer.start()
        defer { hub.stop() }
        let (coordinator, store, cleanup) = makePairingGateCoordinator(suite: "pair-setter")
        defer { cleanup() }
        let link = pairLink(code: "a", name: "Hub+S", hub: hub)

        // Setter first, nothing else: the item is hidden, still pending, still claimable, and not claimed.
        let firstClaim = Task {
            try await coordinator.handleBridgeCall(op: "pair.claim", args: ["link": link], platform: "macos")
        }
        let first = try await waitForPendingPairConfirmation(on: coordinator)
        XCTAssertTrue(coordinator.isPairConfirmationPresented)
        coordinator.hidePairConfirmationAlert()
        await drainMainActor()
        XCTAssertFalse(coordinator.isPairConfirmationPresented)
        XCTAssertEqual(coordinator.pendingExternalPairConfirmation?.id, first.id)
        XCTAssertEqual(hub.pairCallCount(), 0)

        // The Pair button's action arrives in a later turn than the setter: it still claims.
        coordinator.beginConfirmExternalPairing(id: first.id)
        let firstResult = try await awaitWithTimeout(seconds: 10.0) { try await firstClaim.value }
        XCTAssertTrue(firstResult.isEmpty)
        XCTAssertEqual(hub.pairCallCount(), 1)
        XCTAssertEqual(store.activeProfile.name, "Hub S")
        XCTAssertEqual(store.credential(for: store.activeHubID), "cli_paired.secret_cred")
        XCTAssertNil(coordinator.pendingExternalPairConfirmation)

        // The other order: the Pair button's action first, the setter after it.
        let secondClaim = Task {
            try await coordinator.handleBridgeCall(op: "pair.claim", args: ["link": link], platform: "macos")
        }
        let second = try await waitForPendingPairConfirmation(on: coordinator)
        coordinator.beginConfirmExternalPairing(id: second.id)
        coordinator.hidePairConfirmationAlert()
        _ = try await awaitWithTimeout(seconds: 10.0) { try await secondClaim.value }
        XCTAssertEqual(hub.pairCallCount(), 2)
    }

    @MainActor
    func testPairAlertCancelByIDResolvesPairClaimEmptyInEitherOrder() async throws {
        let hub = try await MockUpstreamServer.start()
        defer { hub.stop() }
        let (coordinator, store, cleanup) = makePairingGateCoordinator(suite: "pair-cancel")
        defer { cleanup() }
        let link = pairLink(code: "b", name: "Hub+C", hub: hub)

        // Setter, then (a turn later) the Cancel button.
        let firstClaim = Task {
            try await coordinator.handleBridgeCall(op: "pair.claim", args: ["link": link], platform: "macos")
        }
        let first = try await waitForPendingPairConfirmation(on: coordinator)
        coordinator.hidePairConfirmationAlert()
        await drainMainActor()
        XCTAssertEqual(coordinator.pendingExternalPairConfirmation?.id, first.id)
        // Cancel for another id answers nothing.
        coordinator.cancelPendingExternalPairing(id: UUID())
        XCTAssertEqual(coordinator.pendingExternalPairConfirmation?.id, first.id)
        coordinator.cancelPendingExternalPairing(id: first.id)
        let firstResult = try await awaitWithTimeout(seconds: 10.0) { try await firstClaim.value }
        XCTAssertTrue(firstResult.isEmpty)
        XCTAssertNil(coordinator.pendingExternalPairConfirmation)
        XCTAssertNil(coordinator.externalPairErrorMessage)

        // Cancel button first, setter after it.
        let secondClaim = Task {
            try await coordinator.handleBridgeCall(op: "pair.claim", args: ["link": link], platform: "macos")
        }
        let second = try await waitForPendingPairConfirmation(on: coordinator)
        coordinator.cancelPendingExternalPairing(id: second.id)
        coordinator.hidePairConfirmationAlert()
        let secondResult = try await awaitWithTimeout(seconds: 10.0) { try await secondClaim.value }
        XCTAssertTrue(secondResult.isEmpty)

        // Nothing was claimed, and a pairing after the cancels works.
        XCTAssertEqual(hub.pairCallCount(), 0)
        XCTAssertEqual(store.profiles.count, 1)
        let thirdClaim = Task {
            try await coordinator.handleBridgeCall(op: "pair.claim", args: ["link": link], platform: "macos")
        }
        let third = try await waitForPendingPairConfirmation(on: coordinator)
        coordinator.beginConfirmExternalPairing(id: third.id)
        _ = try await awaitWithTimeout(seconds: 10.0) { try await thirdClaim.value }
        XCTAssertEqual(hub.pairCallCount(), 1)
        XCTAssertEqual(store.activeProfile.name, "Hub C")
    }

    @MainActor
    func testPendingPairingNotOnScreenIsReplacedAndItsPairClaimIsRejected() async throws {
        let hubX = try await MockUpstreamServer.start()
        defer { hubX.stop() }
        let hubY = try await MockUpstreamServer.start()
        defer { hubY.stop() }
        let (coordinator, store, cleanup) = makePairingGateCoordinator(suite: "pair-replace")
        defer { cleanup() }
        let linkX = pairLink(code: "c", name: "Hub+X", hub: hubX)
        let linkY = pairLink(code: "d", name: "Hub+Y", hub: hubY)

        func expectRejected(
            _ task: Task<[String: Any], Error>,
            _ what: String
        ) async {
            do {
                let value = try await task.value
                XCTFail("\(what): pair.claim must be rejected, it resolved \(value)")
            } catch let error as BridgeCallError {
                guard case .operationFailed(let message) = error else {
                    return XCTFail("\(what): unexpected error \(error)")
                }
                XCTAssertFalse(message.isEmpty, what)
                XCTAssertEqual(error.localizedDescription, message, what)
            } catch {
                XCTFail("\(what): unexpected error \(error)")
            }
        }

        // 1. The alert was asked for but the system never showed it (the probe says so). The next
        //    request, a deep link, replaces the item; the first `pair.claim` is rejected, not resolved.
        var alertOnScreen = false
        coordinator.pairConfirmationVisibilityProbe = { alertOnScreen }
        let neverShownClaim = Task {
            try await coordinator.handleBridgeCall(op: "pair.claim", args: ["link": linkX], platform: "macos")
        }
        let neverShown = try await waitForPendingPairConfirmation(on: coordinator)
        XCTAssertTrue(coordinator.isPairConfirmationPresented)
        XCTAssertNil(coordinator.pairingRefusalMessage)
        coordinator.externalPairErrorMessage = "left over from an earlier pairing"
        coordinator.requestExternalPairingConfirmation(uri: linkY)
        await expectRejected(neverShownClaim, "never shown")
        let replacement = try XCTUnwrap(coordinator.pendingExternalPairConfirmation)
        XCTAssertNotEqual(replacement.id, neverShown.id)
        XCTAssertEqual(replacement.displayName, "Hub Y")
        XCTAssertTrue(coordinator.isPairConfirmationPresented)
        // A new pairing starts clean: the old error is gone.
        XCTAssertNil(coordinator.externalPairErrorMessage)
        // The replaced id answers nothing any more.
        let stale = await coordinator.confirmPendingExternalPairing(id: neverShown.id)
        XCTAssertNil(stale)
        XCTAssertEqual(hubX.pairCallCount(), 0)

        // 2. Now the alert for Hub Y is on screen: every entry point is refused and Hub Y stays.
        alertOnScreen = true
        XCTAssertNotNil(coordinator.pairingRefusalMessage)
        let refusedClaim = Task {
            try await coordinator.handleBridgeCall(op: "pair.claim", args: ["link": linkX], platform: "macos")
        }
        await expectRejected(refusedClaim, "on screen")
        coordinator.requestExternalPairingConfirmation(uri: linkX)
        XCTAssertFalse(coordinator.stageScannedPairingCode(linkX))
        XCTAssertEqual(coordinator.pendingExternalPairConfirmation?.id, replacement.id)
        XCTAssertNil(coordinator.externalPairErrorMessage)

        // 3. The alert went away without an answer (only the setter ran). The item no longer blocks:
        //    a bridge request replaces it, and a further one replaces that and rejects its `pair.claim`.
        coordinator.hidePairConfirmationAlert()
        alertOnScreen = false
        XCTAssertNil(coordinator.pairingRefusalMessage)
        let hiddenClaim = Task {
            try await coordinator.handleBridgeCall(op: "pair.claim", args: ["link": linkY], platform: "macos")
        }
        var hidden = try await waitForPendingPairConfirmation(on: coordinator)
        for _ in 0..<2000 where hidden.id == replacement.id {
            try await Task.sleep(nanoseconds: 5_000_000)
            hidden = try XCTUnwrap(coordinator.pendingExternalPairConfirmation)
        }
        XCTAssertNotEqual(hidden.id, replacement.id)
        // Without a probe the flag alone decides: hidden means replaceable.
        coordinator.pairConfirmationVisibilityProbe = nil
        coordinator.hidePairConfirmationAlert()
        XCTAssertTrue(coordinator.stageScannedPairingCode(linkX))
        await expectRejected(hiddenClaim, "hidden")
        let last = try XCTUnwrap(coordinator.pendingExternalPairConfirmation)
        XCTAssertEqual(last.displayName, "Hub X")
        // ... and with the flag up and no probe, the item on screen is not replaced.
        XCTAssertNotNil(coordinator.pairingRefusalMessage)
        coordinator.requestExternalPairingConfirmation(uri: linkY)
        XCTAssertEqual(coordinator.pendingExternalPairConfirmation?.id, last.id)

        // Only the item that survived is claimed.
        let claimed = await coordinator.confirmPendingExternalPairing(id: last.id)
        XCTAssertEqual(claimed?.name, "Hub X")
        XCTAssertEqual(hubX.pairCallCount(), 1)
        XCTAssertEqual(hubY.pairCallCount(), 0)
        XCTAssertEqual(store.activeProfile.name, "Hub X")
    }

    @MainActor
    func testHubsAddWithRedirectingWebPagePortResolvesAndStoresAPIOriginViaDefaultProbe() async throws {
        let apiHub = try await MockUpstreamServer.start()
        defer { apiHub.stop() }
        let webPagePortServer = try await MockUpstreamServer.start(webFrontendTargetHubURL: apiHub.baseURL)
        defer { webPagePortServer.stop() }

        let suiteName = "test.steve.webpage-port.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let store = HubStore(
            defaults: defaults,
            credentials: InMemoryCredentialStore(),
            includeLocalDefault: false
        )
        // Uses default `EndpointResolver()` (`EndpointResolver.defaultProbe`, no injected fetch/probe)
        let coordinator = SteveClientCoordinator(
            hubStore: store,
            preferredPortDefaults: defaults,
            clientDescriptor: ClientDescriptor(kind: "mac", name: "Test Mac", platform: "macOS", version: "0.3.0")
        )
        defer { coordinator.stop() }

        _ = try await coordinator.handleBridgeCall(
            op: "hubs.add",
            args: ["address": webPagePortServer.baseURL.absoluteString, "name": "Split Web Hub"],
            platform: "macos"
        )

        let saved = try XCTUnwrap(store.currentProfile)
        XCTAssertEqual(saved.name, "Split Web Hub")
        XCTAssertEqual(
            saved.orderedEndpoints.map { URLNormalizer.canonicalOrigin($0) },
            [URLNormalizer.canonicalOrigin(apiHub.baseURL)],
            "hubs.add given a 307-redirecting web-only port must store the API port from /hub.json, not the web port"
        )
        XCTAssertEqual(
            URLNormalizer.canonicalOrigin(try XCTUnwrap(coordinator.proxy.currentTarget().baseURL)),
            URLNormalizer.canonicalOrigin(apiHub.baseURL)
        )
    }

    @MainActor
    func testHubSwitchReloadsImmediatelyRotatesProxyTokenAndReportsActiveEndpointInUse() async throws {
        let hubA = try await MockUpstreamServer.start()
        defer { hubA.stop() }
        let hubB = try await MockUpstreamServer.start()
        defer { hubB.stop() }
        // Hub B receives `/api/state` but does not answer it until released: the test looks at the
        // coordinator while that request is known to be in flight.
        hubB.holdStateResponses()

        let suiteName = "test.steve.switch-isolation.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let creds = InMemoryCredentialStore()
        let store = HubStore(
            defaults: defaults,
            credentials: creds,
            defaultProfile: HubProfile(id: HubProfile.localDefaultID, name: "Hub A", lanURLs: [hubA.baseURL], isLocalDefault: true),
            includeLocalDefault: true
        )
        creds.setCredential("cli_a.secret", for: HubProfile.localDefaultID)

        // Hub B has an unreachable public URL (`127.0.0.1:9`) and a reachable LAN fallback (`hubB.baseURL`),
        // plus a held `/api/state` response so we can inspect state mid-`fetchStateSummary`.
        let hubBProfile = HubProfile(
            id: "hub-b",
            name: "Hub B",
            publicURL: URL(string: "https://unreachable.example.invalid:9")!,
            lanURLs: [hubB.baseURL],
            isLocalDefault: false
        )
        let resolver = EndpointResolver { url in
            if url.host == "unreachable.example.invalid" {
                throw URLError(.cannotConnectToHost)
            }
            return try await EndpointResolver.defaultProbe(url)
        }
        _ = store.upsertProfile(hubBProfile, makeActive: false)
        creds.setCredential("cli_b.secret", for: hubBProfile.id)

        let coordinator = SteveClientCoordinator(
            hubStore: store,
            resolver: resolver,
            preferredPortDefaults: defaults,
            clientDescriptor: ClientDescriptor(kind: "mac", name: "Test Mac", platform: "macOS", version: "0.3.0")
        )
        defer { coordinator.stop() }

        _ = try await coordinator.proxy.ensureRunning()
        await coordinator.activateCurrentHub(forceReloadWeb: true)

        let pageATokenCookie = coordinator.proxy.authCookieHeader
        let nonceBeforeSwitch = coordinator.webReloadNonce

        // Begin switching to Hub B (whose `/api/state` is held)
        let switchTask = Task {
            _ = try await coordinator.handleBridgeCall(op: "hubs.switch", args: ["id": hubBProfile.id], platform: "macos")
        }

        // `fetchStateSummary` for Hub B is in flight and stays so until the release below. At this point:
        // 1) the proxy points at Hub B and `webReloadNonce` has ALREADY changed, so the WebView reloads
        //    without waiting for the state summary
        try await waitUntil("Hub B has received /api/state") { hubB.stateCallCount() >= 1 }
        XCTAssertEqual(coordinator.proxy.currentTarget().baseURL, hubB.baseURL)
        XCTAssertNotEqual(
            coordinator.webReloadNonce,
            nonceBeforeSwitch,
            "webReloadNonce must update when the proxy target changes, before fetchStateSummary completes"
        )

        // 2) Any lingering request from Hub A's page carrying `pageATokenCookie` MUST be refused with 403 and never reach Hub B
        let hubBCountBefore = hubB.apiMeCallCount()
        let (_, oldGenResp) = try await RawTCPHTTPClient.request(
            port: coordinator.proxy.port,
            path: "/api/me",
            headers: [
                ("Host", "127.0.0.1:\(coordinator.proxy.port)"),
                ("Cookie", pageATokenCookie),
            ]
        )
        XCTAssertEqual(oldGenResp.status, 403, "Requests carrying the previous Hub page generation's proxy cookie must be rejected")
        XCTAssertEqual(hubB.apiMeCallCount(), hubBCountBefore)

        hubB.releaseStateResponses()
        try await awaitWithTimeout(seconds: 30.0) { try await switchTask.value }

        // 3) `hubs.list` `address` contract: when public URL failed and LAN fallback is in use, `address` is the active LAN endpoint
        let hubsListResult = try await coordinator.handleBridgeCall(op: "hubs.list", args: [:], platform: "macos")
        let hubsList = try XCTUnwrap(hubsListResult["hubs"] as? [[String: Any]])
        let hubBItem = try XCTUnwrap(hubsList.first(where: { ($0["id"] as? String) == hubBProfile.id }))
        XCTAssertEqual(
            hubBItem["address"] as? String,
            URLNormalizer.canonicalOrigin(hubB.baseURL),
            "NativeHub.address must report the endpoint currently in use rather than the unreachable publicURL"
        )

        // 4) Overlay route contract: 100.64.0.0/10 active Hub reports wire code `"tailnet"` in `hubs.list`
        let overlayProfile = HubProfile(
            id: "hub-overlay",
            name: "Overlay Hub",
            lanURLs: [URL(string: "http://100.64.12.34:8440")!],
            isLocalDefault: false
        )
        _ = store.upsertProfile(overlayProfile, makeActive: true)
        // Its address is outside loopback, so its API client gets a session that answers locally: the
        // `/api/state` request the activation makes is seen by the stub and goes nowhere.
        let overlayCoord = SteveClientCoordinator(
            hubStore: store,
            api: HubClientAPI(session: NoNetworkURLProtocol.session()),
            resolver: EndpointResolver { _ in
                HubHelloResponse(name: "steve", version: "0.3.0", api: 1, login: .init(token: true, oidc: false))
            },
            preferredPortDefaults: defaults,
            clientDescriptor: ClientDescriptor(kind: "mac", name: "Test Mac", platform: "macOS", version: "0.3.0")
        )
        defer { overlayCoord.stop() }
        await overlayCoord.activateCurrentHub(forceReloadWeb: false)
        let overlayHubsResult = try await overlayCoord.handleBridgeCall(op: "hubs.list", args: [:], platform: "macos")
        let overlayHubs = try XCTUnwrap(overlayHubsResult["hubs"] as? [[String: Any]])
        let overlayItem = try XCTUnwrap(overlayHubs.first(where: { ($0["id"] as? String) == "hub-overlay" }))
        XCTAssertEqual(overlayItem["route"] as? String, "tailnet")
        XCTAssertEqual(
            NoNetworkURLProtocol.requests(toHost: "100.64.12.34").map(\.path),
            ["/api/state"],
            "the overlay Hub's state request must be answered by the stub session"
        )
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
        // Foreign local server explicitly sends `Access-Control-Allow-Origin: *` so `fetch()` from the gateway origin
        // would succeed with HTTP 200 if `WKContentRuleList` were absent or broken.
        let foreignLocalServer = try await MockUpstreamServer.start(allowWildcardCORS: true)
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

        coordinator.startIfNeeded()
        let gwDeadline = Date().addingTimeInterval(5.0)
        while coordinator.localGatewayURL == nil && Date() < gwDeadline {
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        let gateway = try XCTUnwrap(coordinator.localGatewayURL)

        // 1. Verify `makeProxyAuthCookie` produces a true `HttpOnly` cookie
        let cookie = try XCTUnwrap(SteveWebView.makeProxyAuthCookie(gateway: gateway, token: coordinator.proxy.proxyAuthToken))
        XCTAssertTrue(cookie.isHTTPOnly, "steve_proxy_key must be HttpOnly so document.cookie cannot read it")
        XCTAssertEqual(cookie.name, HubLoopbackProxy.proxyAuthCookieName)

        // 2. Exercise `SteveWebView`'s real `buildWebView` + `loadIfReady` install path (rule list + bootstrap + HttpOnly cookie)
        let steveWebView = SteveWebView(
            coordinator: coordinator,
            platformName: "macos",
            useNonPersistentDataStore: true
        )
        let bridge = steveWebView.makeCoordinator()
        let webView = steveWebView.makeTestWebView(bridge: bridge)

        // Generous 15s bounded wait for cold macos-15 runners
        let deadline = Date().addingTimeInterval(15.0)
        while !bridge.isPageLoaded && Date() < deadline {
            try await Task.sleep(nanoseconds: 50_000_000)
        }
        XCTAssertTrue(bridge.isPageLoaded, "SteveWebView should finish loading bundled index.html via buildWebView + loadIfReady")
        XCTAssertTrue(bridge.hasInstalledRuleList)

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

        // 5. Cross-port fetch to `http://127.0.0.1:<foreignPort>/auth/hello` (which serves `Access-Control-Allow-Origin: *`)
        //    is blocked solely by `WKContentRuleList` before touching the socket.
        let crossPortBlocked = try await webView.callAsyncJavaScript(
            """
            try {
              const r = await fetch(targetURL);
              return !r.ok;
            } catch (_) {
              return true;
            }
            """,
            arguments: ["targetURL": foreignLocalServer.baseURL.appendingPathComponent("auth/hello").absoluteString],
            in: nil,
            contentWorld: .page
        ) as? Bool
        XCTAssertEqual(crossPortBlocked, true, "WKContentRuleList must block fetches to CORS-permissive foreign ports on 127.0.0.1")
        XCTAssertEqual(foreignLocalServer.requestCount(), 0, "Blocked cross-port loopback request must never hit the foreign socket")
    }

    @MainActor
    func testSteveWebViewFailsClosedWhenContentRuleListCompilationFails() async throws {
        let tempDir = FileManager.default.temporaryDirectory.appendingPathComponent("steve-wv-failclosed-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tempDir) }
        try Data("<!doctype html><html><body>should-never-load</body></html>".utf8)
            .write(to: tempDir.appendingPathComponent("index.html"))

        let mockHub = try await MockUpstreamServer.start()
        defer { mockHub.stop() }

        let suiteName = "test.steve.webview-failclosed.\(UUID().uuidString)"
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

        coordinator.startIfNeeded()
        let gwDeadline = Date().addingTimeInterval(5.0)
        while coordinator.localGatewayURL == nil && Date() < gwDeadline {
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        _ = try XCTUnwrap(coordinator.localGatewayURL)

        let failingWebView = SteveWebView(
            coordinator: coordinator,
            platformName: "macos",
            useNonPersistentDataStore: true,
            ruleListCompiler: { _ in
                throw BridgeCallError.operationFailed("synthetic rule compile failure")
            }
        )
        let bridge = failingWebView.makeCoordinator()
        let webView = failingWebView.makeTestWebView(bridge: bridge)

        let deadline = Date().addingTimeInterval(5.0)
        while coordinator.proxyStartupError == nil && Date() < deadline {
            try await Task.sleep(nanoseconds: 25_000_000)
        }

        XCTAssertNil(coordinator.localGatewayURL, "Fail-closed isolation failure must tear down localGatewayURL so the gateway-failure UI renders")
        XCTAssertNotNil(coordinator.proxyStartupError)
        XCTAssertFalse(bridge.isPageLoaded, "Page must never load when WKContentRuleList compilation fails")
        XCTAssertFalse(bridge.hasInstalledRuleList)
        XCTAssertTrue(webView.configuration.userContentController.userScripts.isEmpty, "Bridge bootstrap script must not remain installed on isolation failure")

        let plantedCookies = await withCheckedContinuation { (cont: CheckedContinuation<[HTTPCookie], Never>) in
            webView.configuration.websiteDataStore.httpCookieStore.getAllCookies { cont.resume(returning: $0) }
        }
        XCTAssertFalse(
            plantedCookies.contains(where: { $0.name == HubLoopbackProxy.proxyAuthCookieName }),
            "steve_proxy_key must not be planted when rule list compilation fails"
        )
    }

    // MARK: - The previous Hub's document is gone before the rotated key is planted

    @MainActor
    func testHubSwitchUnloadsPreviousDocumentBeforePlantingRotatedProxyCookieInWebView() async throws {
        let tempDir = FileManager.default.temporaryDirectory.appendingPathComponent("steve-wv-unload-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tempDir) }
        // Every document gets its own id and keeps calling the Hub with it from a timer, as a page that
        // polls does. A call that carries document A's id can only come from Hub A's page.
        try Data("""
        <!doctype html><html><body><div id='app'>ready</div><script>
        window.__docID = Math.random().toString(36).slice(2) + Date.now().toString(36);
        setInterval(function () { fetch('/api/tick/' + window.__docID).catch(function () {}); }, 5);
        </script></body></html>
        """.utf8).write(to: tempDir.appendingPathComponent("index.html"))

        let hubA = try await MockUpstreamServer.start()
        defer { hubA.stop() }
        let hubB = try await MockUpstreamServer.start()
        defer { hubB.stop() }

        let suiteName = "test.steve.webview-unload.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        let store = HubStore(
            defaults: defaults,
            credentials: InMemoryCredentialStore(),
            defaultProfile: HubProfile(id: HubProfile.localDefaultID, name: "Hub A", lanURLs: [hubA.baseURL], isLocalDefault: true),
            includeLocalDefault: true
        )
        let hubBProfile = try XCTUnwrap(store.addManualHub(address: hubB.baseURL.absoluteString, name: "Hub B", makeActive: false))
        let coordinator = SteveClientCoordinator(
            hubStore: store,
            webRoot: tempDir,
            preferredPortDefaults: defaults,
            clientDescriptor: ClientDescriptor(kind: "mac", name: "Test Mac", platform: "macOS", version: "0.3.0")
        )
        defer { coordinator.stop() }

        coordinator.startIfNeeded()
        try await waitUntil("the gateway is up") { coordinator.localGatewayURL != nil }

        let steveWebView = SteveWebView(
            coordinator: coordinator,
            platformName: "macos",
            useNonPersistentDataStore: true
        )
        let bridge = steveWebView.makeCoordinator()

        func proxyKeysInCookieStore(of webView: WKWebView) async -> [String] {
            await withCheckedContinuation { (cont: CheckedContinuation<[String], Never>) in
                webView.configuration.websiteDataStore.httpCookieStore.getAllCookies { cookies in
                    cont.resume(returning: cookies
                        .filter { $0.name == HubLoopbackProxy.proxyAuthCookieName }
                        .map(\.value))
                }
            }
        }

        struct Observation: Equatable {
            let step: SteveWebView.BridgeCoordinator.LoadStep
            let webViewURL: String
            let documentURL: String
            let keysInCookieStore: [String]
        }
        var observations: [Observation] = []
        weak var observedWebView: WKWebView?
        bridge.loadStepObserver = { step in
            guard let webView = observedWebView else { return }
            // Read at the step itself: the load does not go on until this returns.
            let webViewURL = webView.url?.absoluteString ?? ""
            let keys = await proxyKeysInCookieStore(of: webView)
            let documentURL = step == .gatewayLoadStarted
                ? ""
                : ((try? await webView.evaluateJavaScript("document.URL")) as? String ?? "")
            observations.append(Observation(step: step, webViewURL: webViewURL, documentURL: documentURL, keysInCookieStore: keys))
        }

        let webView = steveWebView.makeTestWebView(bridge: bridge)
        observedWebView = webView
        try await waitUntil("Hub A's page is loaded") { bridge.isPageLoaded }
        let tokenA = coordinator.proxy.proxyAuthToken
        let docAValue = try await webView.evaluateJavaScript("window.__docID")
        let docA = try XCTUnwrap(docAValue as? String)
        try await waitUntil("Hub A's page reaches Hub A from its timer") {
            hubA.receivedPaths().contains("/api/tick/\(docA)")
        }

        // Switch to Hub B. The key is rotated at once; the WebView is told as SwiftUI would tell it.
        _ = try await coordinator.handleBridgeCall(op: "hubs.switch", args: ["id": hubBProfile.id], platform: "macos")
        let tokenB = coordinator.proxy.proxyAuthToken
        XCTAssertNotEqual(tokenA, tokenB)
        observations.removeAll()
        steveWebView.updateTestWebView(webView, bridge: bridge)
        XCTAssertFalse(bridge.isPageLoaded)
        try await waitUntil("Hub B's page is loaded") {
            bridge.isPageLoaded && observations.contains(where: { $0.step == .gatewayLoadStarted })
        }

        // The order, and what was true at each step.
        XCTAssertEqual(
            observations.map(\.step),
            [.previousDocumentUnloaded, .proxyCookiePlanted, .gatewayLoadStarted]
        )
        let unloaded = try XCTUnwrap(observations.first(where: { $0.step == .previousDocumentUnloaded }))
        XCTAssertEqual(unloaded.webViewURL, "about:blank")
        XCTAssertEqual(unloaded.documentURL, "about:blank", "Hub A's document must be gone before the new key is planted")
        XCTAssertEqual(unloaded.keysInCookieStore, [tokenA], "the rotated key must not be in the cookie store while Hub A's document can still be alive")
        let planted = try XCTUnwrap(observations.first(where: { $0.step == .proxyCookiePlanted }))
        XCTAssertEqual(planted.keysInCookieStore, [tokenB])
        XCTAssertEqual(planted.documentURL, "about:blank", "the new key is planted while no Hub page is loaded")

        // Hub B's page is a new document, it reaches Hub B, and nothing of Hub A's document ever did.
        let docBValue = try await webView.evaluateJavaScript("window.__docID")
        let docB = try XCTUnwrap(docBValue as? String)
        XCTAssertNotEqual(docA, docB)
        try await waitUntil("Hub B's page reaches Hub B from its timer") {
            hubB.receivedPaths().contains("/api/tick/\(docB)")
        }
        XCTAssertFalse(
            hubB.receivedPaths().contains("/api/tick/\(docA)"),
            "a timer of Hub A's page must never reach Hub B"
        )
        XCTAssertNil(coordinator.proxyStartupError)
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
        // Every resolve waits at a gate the test opens, so "the poll is still resolving" is a fact the
        // test establishes, not a guess about how long things take.
        let resolveGate = TestGate()
        defer { resolveGate.open() }
        let slowResolver = EndpointResolver { _ in
            await resolveGate.pass()
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
        try await waitUntil("the poll is inside the resolver") { resolveGate.arrivalCount >= 1 }

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
        // The sign-in has reached the store while the poll is still held in the resolver.
        try await waitUntil("the sign-in is stored") {
            store.credential(for: HubProfile.localDefaultID) == "cli_web_tok.secret_cred"
        }

        resolveGate.open()
        await inFlightPoll.value

        // The stale poll has now finished: it must NOT have clobbered the newly stored credential back to nil!
        XCTAssertEqual(store.credential(for: HubProfile.localDefaultID), "cli_web_tok.secret_cred")
        try await waitUntil("the refresh after the sign-in gave the proxy the new credential") {
            coordinator.proxy.currentTarget().credential == "cli_web_tok.secret_cred"
        }
        XCTAssertEqual(store.credential(for: HubProfile.localDefaultID), "cli_web_tok.secret_cred")

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
        try await waitUntil("the replaced credential is signed out at the Hub") {
            mockHub.lastLogoutAuth() == "Bearer cli_old.stale_secret"
        }
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
        let evilPending = try XCTUnwrap(coordinator.pendingExternalPairConfirmation)
        XCTAssertEqual(store.profiles.count, 1)

        await coordinator.confirmPendingExternalPairing(id: evilPending.id)
        XCTAssertNotNil(coordinator.externalPairErrorMessage)
        XCTAssertEqual(store.profiles.count, 1)
        XCTAssertEqual(store.credential(for: HubProfile.localDefaultID), "cli_local_owner.top_secret")

        let validPubEncoded = realNewHub.baseURL.absoluteString.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed)!
        let overlappingURI = "steve://pair?v=1&code=stp_fedcba9876543210fedcba9876543210&name=PairedHub&pub=\(validPubEncoded)&lan=\(lanLocalEncoded)"
        coordinator.requestExternalPairingConfirmation(uri: overlappingURI)
        let validPending = try XCTUnwrap(coordinator.pendingExternalPairConfirmation)
        let confirmedProfile = await coordinator.confirmPendingExternalPairing(id: validPending.id)
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
        let pending = try XCTUnwrap(coordinator.pendingExternalPairConfirmation)

        let result = await coordinator.confirmPendingExternalPairing(id: pending.id)
        XCTAssertNil(result)
        XCTAssertFalse(store.hasProfiles)
        XCTAssertEqual(coordinator.externalPairErrorMessage, CredentialStoreError.saveFailed.localizedDescription)
    }

    // MARK: - Restored Coverage: Gateway Origin Confinement, Stale Activation Race & Web Logout Sync

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

    @MainActor
    func testStaleActivationDoesNotOverwriteSwitchAndInactiveRemoveDoesNotReload() async throws {
        let mockHubA = try await MockUpstreamServer.start()
        defer { mockHubA.stop() }
        let mockHubB = try await MockUpstreamServer.start()
        defer { mockHubB.stop() }

        let suiteName = "test.steve.race.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defer { defaults.removePersistentDomain(forName: suiteName) }

        // "Slow A" is a Hub on loopback whose resolve waits at a gate the test opens.
        let slowURL = mockHubA.baseURL
        let hello = HubHelloResponse(name: "steve", version: "0.3.0", api: 1, login: .init(token: true, oidc: false))
        let slowGate = TestGate()
        defer { slowGate.open() }
        let resolver = EndpointResolver { candidate in
            if candidate == slowURL {
                await slowGate.pass()
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
        try await waitUntil("the poll is resolving Slow A") { slowGate.arrivalCount >= 1 }
        store.selectHub(id: hubB.id)
        await coordinator.activateCurrentHub(forceReloadWeb: true)
        // Only now does Slow A answer: the poll that asked is stale.
        slowGate.open()
        await stalePoll.value

        XCTAssertEqual(coordinator.proxy.currentTarget().hubID, hubB.id)
        XCTAssertEqual(coordinator.proxy.currentTarget().baseURL, mockHubB.baseURL)
        XCTAssertEqual(coordinator.summary.activeEndpoint, mockHubB.baseURL)

        // Switch back to Slow A, then remove inactive Hub B -> must NOT change `webReloadNonce`, and must call `/auth/client/logout`
        store.selectHub(id: HubProfile.localDefaultID)
        await coordinator.activateCurrentHub(forceReloadWeb: true)
        let nonceBeforeRemove = coordinator.webReloadNonce

        coordinator.removeHub(id: hubB.id)
        // The sign-out is sent at the end of the removal, so once the Hub has it the removal is done.
        try await waitUntil("the removed Hub's credential is signed out there") {
            mockHubB.lastLogoutAuth() == "Bearer cli_b.secret"
        }
        XCTAssertFalse(store.profiles.contains(where: { $0.id == hubB.id }))
        XCTAssertEqual(coordinator.webReloadNonce, nonceBeforeRemove, "Removing an inactive Hub must not reload active WebView")
        XCTAssertEqual(store.activeHubID, HubProfile.localDefaultID)
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
        try await waitUntil("the web sign-in reached the store, the proxy and the summary") {
            store.credential(for: HubProfile.localDefaultID) == "cli_web_tok.secret_cred"
                && coordinator.proxy.currentTarget().credential == "cli_web_tok.secret_cred"
                && coordinator.summary.phase == .connected
        }

        XCTAssertEqual(store.credential(for: HubProfile.localDefaultID), "cli_web_tok.secret_cred")
        XCTAssertEqual(coordinator.proxy.currentTarget().credential, "cli_web_tok.secret_cred")
        XCTAssertEqual(coordinator.summary.phase, .connected)

        // Web UI logging out via `POST /auth/client/logout` clears native stored credential & proxy target credential
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
        try await waitUntil("the web sign-out reached the store, the proxy and the summary") {
            store.credential(for: HubProfile.localDefaultID) == nil
                && coordinator.proxy.currentTarget().credential == nil
                && coordinator.summary.phase == .authRequired
        }

        XCTAssertNil(store.credential(for: HubProfile.localDefaultID))
        XCTAssertNil(coordinator.proxy.currentTarget().credential)
        XCTAssertEqual(coordinator.summary.phase, .authRequired)
    }

    // MARK: - Bounded Async Test Helpers

    @MainActor
    private func waitForPendingPairConfirmation(
        on coordinator: SteveClientCoordinator,
        timeoutSeconds: TimeInterval = 5.0
    ) async throws -> PendingPairConfirmation {
        let deadline = Date().addingTimeInterval(timeoutSeconds)
        while Date() < deadline {
            if let pending = coordinator.pendingExternalPairConfirmation {
                return pending
            }
            try await Task.sleep(nanoseconds: 15_000_000)
        }
        XCTFail("Timed out waiting for pendingExternalPairConfirmation")
        throw URLError(.timedOut)
    }

    /// Polls for `condition` up to a generous deadline. Used instead of "sleep, then assert": the wait
    /// ends as soon as the condition holds, and a slow machine only makes it longer, not wrong.
    @MainActor
    private func waitUntil(
        _ what: String,
        timeoutSeconds: TimeInterval = 30.0,
        file: StaticString = #filePath,
        line: UInt = #line,
        _ condition: @MainActor () async -> Bool
    ) async throws {
        let deadline = Date().addingTimeInterval(timeoutSeconds)
        while true {
            if await condition() { return }
            if Date() >= deadline { break }
            try await Task.sleep(nanoseconds: 5_000_000)
        }
        XCTFail("Timed out waiting until \(what)", file: file, line: line)
        throw URLError(.timedOut)
    }

    private func awaitWithTimeout<T: Sendable>(
        seconds: TimeInterval,
        operation: @escaping @Sendable () async throws -> T
    ) async throws -> T {
        try await withThrowingTaskGroup(of: T.self) { group in
            group.addTask {
                try await operation()
            }
            group.addTask {
                try await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
                throw URLError(.timedOut)
            }
            let first = try await group.next()!
            group.cancelAll()
            return first
        }
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
                conn.receive(minimumIncompleteLength: 1, maximumLength: 65_536) { data, _, _, err in
                    if let data, !data.isEmpty {
                        cont.resume(returning: data)
                    } else if case .posix(.ENODATA)? = err {
                        // how Network.framework sometimes reports that the peer closed the stream
                        cont.resume(returning: Data())
                    } else if let err {
                        cont.resume(throwing: err)
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
    private var recordedAPIMeCalls = 0
    private var recordedStateCalls = 0
    private var recordedPaths: [String] = []
    private var recordedLogoutCalls = 0
    private var stateResponsesHeld = false
    private var heldStateResponses: [() -> Void] = []

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

    func apiMeCallCount() -> Int {
        lock.lock()
        defer { lock.unlock() }
        return recordedAPIMeCalls
    }

    /// Authorized `/api/state` requests received so far, answered or not.
    func stateCallCount() -> Int {
        lock.lock()
        defer { lock.unlock() }
        return recordedStateCalls
    }

    /// The path of every request received, in order.
    func receivedPaths() -> [String] {
        lock.lock()
        defer { lock.unlock() }
        return recordedPaths
    }

    private func recordPath(_ path: String) {
        lock.lock()
        recordedPaths.append(path)
        lock.unlock()
    }

    func logoutCallCount() -> Int {
        lock.lock()
        defer { lock.unlock() }
        return recordedLogoutCalls
    }

    /// From now on `/api/state` is received and counted but not answered until `releaseStateResponses()`.
    /// A test that has seen `stateCallCount()` go up knows the request is in flight, for as long as it likes.
    func holdStateResponses() {
        lock.lock()
        stateResponsesHeld = true
        lock.unlock()
    }

    func releaseStateResponses() {
        lock.lock()
        stateResponsesHeld = false
        let held = heldStateResponses
        heldStateResponses = []
        lock.unlock()
        held.forEach { $0() }
    }

    private func answerState(_ send: @escaping () -> Void) {
        lock.lock()
        recordedStateCalls += 1
        if stateResponsesHeld {
            heldStateResponses.append(send)
            lock.unlock()
            return
        }
        lock.unlock()
        send()
    }

    private func recordRequest() {
        lock.lock()
        recordedTotalRequests += 1
        lock.unlock()
    }

    private func recordAPIMeCall() {
        lock.lock()
        recordedAPIMeCalls += 1
        lock.unlock()
    }

    private func recordLogout(_ auth: String) {
        lock.lock()
        recordedLogoutAuth = auth
        recordedLogoutCalls += 1
        lock.unlock()
    }

    private func recordPairCall() {
        lock.lock()
        recordedPairCalls += 1
        lock.unlock()
    }

    static func start(
        omitPairCredential: Bool = false,
        webFrontendTargetHubURL: URL? = nil,
        allowWildcardCORS: Bool = false
    ) async throws -> MockUpstreamServer {
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
                    let method = lines.first?.split(separator: " ").first.map(String.init) ?? "GET"
                    let path = lines.first?.split(separator: " ").dropFirst().first.map(String.init) ?? ""
                    box.server?.recordPath(path)
                    let headerVal = { (prefix: String) -> String in
                        lines.first(where: { $0.lowercased().hasPrefix(prefix) })?
                            .split(separator: ":", maxSplits: 1).dropFirst().first?
                            .trimmingCharacters(in: .whitespaces) ?? ""
                    }
                    let auth = headerVal("authorization:")
                    let cookie = headerVal("cookie:")
                    let corsHeaders = allowWildcardCORS
                        ? "Access-Control-Allow-Origin: *\r\nAccess-Control-Allow-Headers: *\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\n"
                        : ""

                    if method == "OPTIONS" && allowWildcardCORS {
                        let head = "HTTP/1.1 204 No Content\r\n\(corsHeaders)Content-Length: 0\r\nConnection: close\r\n\r\n"
                        conn.send(content: Data(head.utf8), completion: .contentProcessed { _ in conn.cancel() })
                        return
                    }

                    // Simulate a Steve web-only port (`STEVE_WEB_PORT`):
                    // `/hub.json` returns `{"hub":"http://127.0.0.1:<apiPort>"}` and `/auth/*` returns `307 Temporary Redirect` to the API port.
                    if let targetHubURL = webFrontendTargetHubURL {
                        if path == "/hub.json" {
                            let body = Data("{\"hub\":\"\(targetHubURL.absoluteString)\"}".utf8)
                            let head = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\(corsHeaders)Content-Length: \(body.count)\r\nConnection: close\r\n\r\n"
                            conn.send(content: Data(head.utf8) + body, completion: .contentProcessed { _ in conn.cancel() })
                            return
                        }
                        if path.hasPrefix("/auth/") {
                            let location = targetHubURL.absoluteString.replacingOccurrences(of: "/+$", with: "", options: .regularExpression) + path
                            let head = "HTTP/1.1 307 Temporary Redirect\r\nLocation: \(location)\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            conn.send(content: Data(head.utf8), completion: .contentProcessed { _ in conn.cancel() })
                            return
                        }
                        let body = Data("<!doctype html><html><body>web-only</body></html>".utf8)
                        let head = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: \(body.count)\r\nConnection: close\r\n\r\n"
                        conn.send(content: Data(head.utf8) + body, completion: .contentProcessed { _ in conn.cancel() })
                        return
                    }

                    if path == "/api/me" {
                        box.server?.recordAPIMeCall()
                    }

                    if path == "/api/stream" {
                        let sseHead = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\(corsHeaders)Cache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n"
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
                            + corsHeaders
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
                            + corsHeaders
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
                        let head = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\(corsHeaders)Content-Length: \(payload.count)\r\nConnection: close\r\n\r\n"
                        conn.send(content: Data(head.utf8) + payload, completion: .contentProcessed { _ in conn.cancel() })
                        return
                    }

                    let sendJSONResponse = { (payload: Data) in
                        let head = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\(corsHeaders)Content-Length: \(payload.count)\r\nConnection: close\r\n\r\n"
                        conn.send(content: Data(head.utf8) + payload, completion: .contentProcessed { _ in conn.cancel() })
                    }

                    if path == "/api/state" {
                        if auth.isEmpty && cookie.isEmpty {
                            let unauthorized = Data("{\"error\":\"unauthorized\"}".utf8)
                            let head = "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\n\(corsHeaders)Content-Length: \(unauthorized.count)\r\nConnection: close\r\n\r\n"
                            conn.send(content: Data(head.utf8) + unauthorized, completion: .contentProcessed { _ in conn.cancel() })
                            return
                        }
                        let payload = Data("""
                        {"version":"0.3.0","web_version":"0.3.0","thinking":true,"workspace":{"name":"Core"},"devices":[{"status":"正常"},{"status":"暂停接活"},{"status":"离线"}],"sessions":[{"status":"running","busy":true},{"status":"idle","busy":true},{"status":"idle","busy":false}],"decisions":[{"status":"open"},{"status":"resolved"}]}
                        """.utf8)
                        if let server = box.server {
                            server.answerState { sendJSONResponse(payload) }
                        } else {
                            sendJSONResponse(payload)
                        }
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
                    } else {
                        payload = try! JSONEncoder().encode(["path": path, "auth": auth, "cookie": cookie])
                    }
                    sendJSONResponse(payload)
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
        releaseStateResponses()
        listener.cancel()
    }
}

/// A door the test holds shut. Callers wait at it and are counted; `open()` lets them, and everyone
/// after them, through.
private final class TestGate: @unchecked Sendable {
    private let lock = NSLock()
    private var isOpen = false
    private var arrivals = 0
    private var waiters: [CheckedContinuation<Void, Never>] = []

    var arrivalCount: Int {
        lock.lock()
        defer { lock.unlock() }
        return arrivals
    }

    func pass() async {
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            lock.lock()
            arrivals += 1
            if isOpen {
                lock.unlock()
                continuation.resume()
            } else {
                waiters.append(continuation)
                lock.unlock()
            }
        }
    }

    func open() {
        lock.lock()
        isOpen = true
        let waiting = waiters
        waiters = []
        lock.unlock()
        waiting.forEach { $0.resume() }
    }
}

/// Answers every request itself with `503`. A `URLSession` built on it never opens a connection, so a
/// coordinator whose Hub has an address outside loopback can be driven without anything leaving the test.
private final class NoNetworkURLProtocol: URLProtocol {
    private static let lock = NSLock()
    private static var seenURLs: [URL] = []

    static func session() -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [NoNetworkURLProtocol.self]
        return URLSession(configuration: configuration)
    }

    /// The requests the stub answered for `host`.
    static func requests(toHost host: String) -> [URL] {
        lock.lock()
        defer { lock.unlock() }
        return seenURLs.filter { $0.host == host }
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let url = request.url else {
            client?.urlProtocol(self, didFailWithError: URLError(.badURL))
            return
        }
        Self.lock.lock()
        Self.seenURLs.append(url)
        Self.lock.unlock()
        let response = HTTPURLResponse(
            url: url,
            statusCode: 503,
            httpVersion: "HTTP/1.1",
            headerFields: ["Content-Length": "0"]
        )!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}
