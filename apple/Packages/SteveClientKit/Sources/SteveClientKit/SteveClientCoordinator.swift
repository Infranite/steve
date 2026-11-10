import Foundation

@MainActor
public final class SteveClientCoordinator: ObservableObject {
    @Published public private(set) var summary = HubRuntimeSummary()
    @Published public private(set) var localGatewayURL: URL?
    @Published public private(set) var webReloadNonce = UUID()
    @Published public private(set) var targetHashRoute: String?

    @Published public var isHubSwitcherPresented = false
    @Published public var isPairQRPresented = false
    @Published public var isQRScannerPresented = false

    public let hubStore: HubStore
    public let api: HubClientAPI
    public let resolver: EndpointResolver
    public let proxy: HubLoopbackProxy
    public let clientDescriptor: ClientDescriptor

    private var monitorTask: Task<Void, Never>?
    private var started = false

    public init(
        hubStore: HubStore? = nil,
        api: HubClientAPI = HubClientAPI(),
        resolver: EndpointResolver = EndpointResolver(),
        webRoot: URL? = nil,
        clientDescriptor: ClientDescriptor
    ) {
        self.hubStore = hubStore ?? HubStore()
        self.api = api
        self.resolver = resolver
        self.proxy = HubLoopbackProxy(webRoot: webRoot)
        self.clientDescriptor = clientDescriptor
    }

    public func startIfNeeded() {
        guard !started else { return }
        started = true
        Task { [weak self] in
            guard let self else { return }
            if let url = try? await self.proxy.start() {
                self.localGatewayURL = url
            }
            await self.activateCurrentHub(forceReloadWeb: true)
            self.startBackgroundPolling()
        }
    }

    public func stop() {
        monitorTask?.cancel()
        monitorTask = nil
        proxy.stop()
        started = false
    }

    public func switchToHub(id: String) {
        guard hubStore.activeHubID != id else { return }
        hubStore.selectHub(id: id)
        Task { [weak self] in
            await self?.activateCurrentHub(forceReloadWeb: true)
        }
    }

    public func reconnectActiveHub() {
        Task { [weak self] in
            await self?.activateCurrentHub(forceReloadWeb: true)
        }
    }

    public func navigate(hashRoute: String) {
        targetHashRoute = hashRoute
    }

    public func consumeTargetHashRoute() {
        targetHashRoute = nil
    }

    public func addAndSwitchManualHub(address: String, name: String?) async throws {
        guard let added = hubStore.addManualHub(address: address, name: name, makeActive: true) else {
            throw EndpointResolutionError.noCandidates
        }
        _ = added
        await activateCurrentHub(forceReloadWeb: true)
    }

    /// Pairs this device (primarily iOS) by resolving the QR payload's public-first / LAN-fallback endpoints
    /// and claiming the single-use `stp_...` code for a dedicated Keychain-stored client credential.
    @discardableResult
    public func pairWithQRCode(uri: String) async throws -> HubProfile {
        guard let payload = PairingPayload.parse(uri) else {
            throw HubClientAPIError.invalidResponse
        }
        let resolved = try await resolver.resolve(pairing: payload)
        let claim = try await api.claimPairing(
            endpoint: resolved.url,
            code: payload.code,
            client: clientDescriptor
        )

        let displayName = payload.name ?? resolved.url.host ?? "Steve Hub"
        let profile = HubProfile(
            name: displayName,
            publicURL: payload.publicURL,
            lanURLs: payload.lanURLs,
            isLocalDefault: false,
            clientID: claim.client_id,
            userName: claim.user.name,
            lastKnownVersion: claim.hub.version,
            lastConnectedAt: Date()
        )
        let saved = hubStore.upsertProfile(profile, credential: claim.credential, makeActive: true)
        await activateCurrentHub(forceReloadWeb: true)
        return saved
    }

    public func signInActiveHubWithToken(user: String, token: String) async throws {
        let profile = hubStore.activeProfile
        let resolved = try await resolver.resolve(profile: profile)
        let signed = try await api.signInWithToken(
            endpoint: resolved.url,
            user: user,
            token: token,
            client: clientDescriptor
        )
        hubStore.updateAuthMetadata(
            id: profile.id,
            clientID: signed.client_id,
            userName: signed.user.name,
            credential: signed.credential
        )
        hubStore.updateConnectionMetadata(id: profile.id, version: signed.hub.version)
        await activateCurrentHub(forceReloadWeb: true)
    }

    public func createPairingForActiveHub() async throws -> CreatedPairing {
        let profile = hubStore.activeProfile
        let endpoint = summary.activeEndpoint ?? profile.orderedEndpoints.first
        guard let endpoint else {
            throw EndpointResolutionError.noCandidates
        }
        return try await api.createPairing(
            endpoint: endpoint,
            credential: hubStore.credential(for: profile.id)
        )
    }

    public func pollPairingStatus(id: String) async throws -> PairingClaimStatus {
        let profile = hubStore.activeProfile
        guard let endpoint = summary.activeEndpoint ?? profile.orderedEndpoints.first else {
            throw EndpointResolutionError.noCandidates
        }
        return try await api.pairingStatus(
            endpoint: endpoint,
            id: id,
            credential: hubStore.credential(for: profile.id)
        )
    }

    // MARK: - Internal Resolution & Status Polling

    public func activateCurrentHub(forceReloadWeb: Bool) async {
        let profile = hubStore.activeProfile
        let credential = hubStore.credential(for: profile.id)

        if forceReloadWeb {
            summary = HubRuntimeSummary(
                phase: .connecting,
                activeEndpoint: profile.orderedEndpoints.first,
                route: profile.orderedEndpoints.first.map { HubRouteKind.classify(url: $0, explicitPublicURL: profile.publicURL) } ?? .unknown,
                hubVersion: profile.lastKnownVersion
            )
        }

        do {
            let resolved = try await resolver.resolve(profile: profile)
            proxy.updateTarget(.init(baseURL: resolved.url, credential: credential))
            hubStore.updateConnectionMetadata(id: profile.id, version: resolved.hello.version)

            if let liveSummary = try? await api.fetchStateSummary(
                endpoint: resolved.url,
                route: resolved.route,
                helloVersion: resolved.hello.version,
                credential: credential
            ) {
                summary = liveSummary
            } else {
                summary = HubRuntimeSummary(
                    phase: .connected,
                    activeEndpoint: resolved.url,
                    route: resolved.route,
                    hubVersion: resolved.hello.version,
                    updatedAt: Date()
                )
            }
        } catch {
            // Even when offline, point the proxy at the primary candidate so user retries reach it once up
            proxy.updateTarget(.init(baseURL: profile.orderedEndpoints.first, credential: credential))
            summary = HubRuntimeSummary(
                phase: .offline,
                activeEndpoint: profile.orderedEndpoints.first,
                route: profile.orderedEndpoints.first.map { HubRouteKind.classify(url: $0, explicitPublicURL: profile.publicURL) } ?? .unknown,
                hubVersion: profile.lastKnownVersion,
                errorMessage: error.localizedDescription,
                updatedAt: Date()
            )
        }

        if forceReloadWeb {
            webReloadNonce = UUID()
        }
    }

    private func startBackgroundPolling() {
        monitorTask?.cancel()
        monitorTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 6_000_000_000)
                guard !Task.isCancelled, let self else { break }
                await self.activateCurrentHub(forceReloadWeb: false)
            }
        }
    }
}
