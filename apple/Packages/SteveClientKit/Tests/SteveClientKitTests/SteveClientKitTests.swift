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

    func testEndpointResolverConnectsViaPublicByDefaultAndFallsBackToLan() async throws {
        let pub = URL(string: "https://steve.example.com")!
        let lan = URL(string: "http://192.168.1.20:8440")!
        let hello = HubHelloResponse(name: "steve", version: "0.3.0", api: 1, login: .init(token: true, oidc: false))

        // Case 1: Both public and LAN reachable -> connects via public by default
        actor AttemptRecorder {
            var seen: [String] = []
            func record(_ url: URL) { seen.append(url.absoluteString) }
            func list() -> [String] { seen }
        }
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

        // Case 2: Public unreachable -> falls back cleanly to LAN
        let rec2 = AttemptRecorder()
        let resolverPubDown = EndpointResolver { candidate in
            await rec2.record(candidate)
            if candidate == pub {
                throw URLError(.cannotConnectToHost)
            }
            return hello
        }
        let res2 = try await resolverPubDown.resolve(publicURL: pub, lanURLs: [lan])
        XCTAssertEqual(res2.url, lan)
        XCTAssertEqual(res2.route, .lan)
        XCTAssertTrue(res2.usedFallbackFromPublic)
        let seen2 = await rec2.list()
        XCTAssertEqual(seen2, ["https://steve.example.com", "http://192.168.1.20:8440"])

        // Case 3: No public URL -> connects via LAN directly
        let res3 = try await resolverBothUp.resolve(publicURL: nil, lanURLs: [lan])
        XCTAssertEqual(res3.url, lan)
        XCTAssertEqual(res3.route, .lan)
        XCTAssertFalse(res3.usedFallbackFromPublic)
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
            defaultProfile: LocalHubDiscovery.defaultProfile(environment: [:])
        )

        // Default on launch: auto-selected local Hub on 127.0.0.1:8440
        XCTAssertEqual(store.activeProfile.id, HubProfile.localDefaultID)
        XCTAssertTrue(store.activeProfile.isLocalDefault)
        XCTAssertEqual(store.activeProfile.orderedEndpoints.map(\.absoluteString), ["http://127.0.0.1:8440"])

        // Add remote Hub & switch
        let added = store.addManualHub(address: "https://remote.example.com/", name: "Remote Hub", makeActive: true)
        XCTAssertNotNil(added)
        XCTAssertEqual(store.activeProfile.name, "Remote Hub")
        XCTAssertEqual(store.activeProfile.publicURL?.absoluteString, "https://remote.example.com")

        // Switch back to local Hub
        store.selectHub(id: HubProfile.localDefaultID)
        XCTAssertEqual(store.activeProfile.id, HubProfile.localDefaultID)

        // Local default cannot be deleted; remote Hub can
        store.removeHub(id: HubProfile.localDefaultID)
        XCTAssertEqual(store.profiles.count, 2)
        if let remoteID = added?.id {
            store.selectHub(id: remoteID)
            store.removeHub(id: remoteID)
        }
        XCTAssertEqual(store.profiles.count, 1)
        XCTAssertEqual(store.activeProfile.id, HubProfile.localDefaultID)
    }

    func testLoopbackProxyServesStaticAssetsAndForwardsBearerCredentials() async throws {
        let tempDir = FileManager.default.temporaryDirectory.appendingPathComponent("steve-proxy-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: tempDir) }

        let indexHTML = "<!doctype html><html><body>Steve Native Bundle</body></html>"
        try Data(indexHTML.utf8).write(to: tempDir.appendingPathComponent("index.html"))

        // Spin up a tiny mock upstream Hub using NWListener
        let mockHub = try await MockUpstreamServer.start()
        defer { mockHub.stop() }

        let proxy = HubLoopbackProxy(webRoot: tempDir)
        proxy.updateTarget(.init(baseURL: mockHub.baseURL, credential: "cli_test.secret_token"))
        let gateway = try await proxy.start()
        defer { proxy.stop() }

        // 1. Static root serves bundled index.html
        let (htmlData, htmlResp) = try await URLSession.shared.data(from: gateway)
        XCTAssertEqual((htmlResp as? HTTPURLResponse)?.statusCode, 200)
        XCTAssertEqual(String(data: htmlData, encoding: .utf8), indexHTML)

        // 2. `/api/me` proxies to upstream with injected `Authorization: Bearer cli_test.secret_token`
        let (apiData, apiResp) = try await URLSession.shared.data(from: gateway.appendingPathComponent("api/me"))
        XCTAssertEqual((apiResp as? HTTPURLResponse)?.statusCode, 200)
        let json = try JSONSerialization.jsonObject(with: apiData) as? [String: String]
        XCTAssertEqual(json?["auth"], "Bearer cli_test.secret_token")
        XCTAssertEqual(json?["path"], "/api/me")

        // 3. `/api/stream` streams SSE events incrementally without waiting for EOF
        let (bytes, sseResp) = try await URLSession.shared.bytes(from: gateway.appendingPathComponent("api/stream"))
        XCTAssertEqual((sseResp as? HTTPURLResponse)?.statusCode, 200)
        XCTAssertEqual((sseResp as? HTTPURLResponse)?.value(forHTTPHeaderField: "Content-Type"), "text/event-stream")
        var receivedLines: [String] = []
        for try await line in bytes.lines {
            receivedLines.append(line)
            if receivedLines.count >= 2 { break }
        }
        XCTAssertEqual(receivedLines, ["id: 1", "data: {\"type\":\"steve.thinking\",\"on\":true}"])
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
            defaultProfile: LocalHubDiscovery.defaultProfile(environment: [:])
        )
        let coordinator = SteveClientCoordinator(
            hubStore: store,
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
}

private final class MockUpstreamServer: @unchecked Sendable {
    let listener: NWListener
    let baseURL: URL
    private let queue = DispatchQueue(label: "test.steve.mock-upstream")

    private init(listener: NWListener, port: UInt16) {
        self.listener = listener
        self.baseURL = URL(string: "http://127.0.0.1:\(port)")!
    }

    static func start() async throws -> MockUpstreamServer {
        let params = NWParameters.tcp
        params.requiredLocalEndpoint = NWEndpoint.hostPort(host: .ipv4(.loopback), port: .any)
        let listener = try NWListener(using: params)

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
                    let auth = lines.first(where: { $0.lowercased().hasPrefix("authorization:") })?
                        .split(separator: ":", maxSplits: 1).dropFirst().first?
                        .trimmingCharacters(in: .whitespaces) ?? ""

                    if path == "/api/stream" {
                        let sseHead = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n"
                        let frame = "id: 1\ndata: {\"type\":\"steve.thinking\",\"on\":true}\n\n"
                        conn.send(content: Data((sseHead + frame).utf8), completion: .contentProcessed { _ in
                            DispatchQueue.global().asyncAfter(deadline: .now() + 1.0) { conn.cancel() }
                        })
                        return
                    }

                    let payload: Data
                    if path == "/auth/hello" {
                        payload = Data("{\"name\":\"steve\",\"version\":\"0.3.0\",\"api\":1,\"login\":{\"token\":true,\"oidc\":false}}".utf8)
                    } else if path == "/auth/pair" {
                        payload = Data("{\"client_id\":\"cli_paired\",\"credential\":\"cli_paired.secret_cred\",\"user\":{\"id\":\"owner\",\"name\":\"Owner\",\"role\":\"owner\"},\"hub\":{\"version\":\"0.3.0\"}}".utf8)
                    } else if path == "/api/state" {
                        payload = Data("""
                        {"version":"0.3.0","web_version":"0.3.0","thinking":true,"workspace":{"name":"Core"},"devices":[{"status":"正常"},{"status":"暂停接活"},{"status":"离线"}],"sessions":[{"status":"running","busy":true},{"status":"idle","busy":true},{"status":"idle","busy":false}],"decisions":[{"status":"open"},{"status":"resolved"}]}
                        """.utf8)
                    } else {
                        payload = try! JSONEncoder().encode(["path": path, "auth": auth])
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
        return MockUpstreamServer(listener: listener, port: port)
    }

    func stop() {
        listener.cancel()
    }
}
