import Foundation
import WebKit

public struct PendingPairConfirmation: Identifiable, Sendable, Equatable {
    public let id: UUID
    public let rawURI: String
    public let payload: PairingPayload

    public init(id: UUID = UUID(), rawURI: String, payload: PairingPayload) {
        self.id = id
        self.rawURI = rawURI
        self.payload = payload
    }

    public var displayName: String {
        payload.name ?? payload.orderedEndpoints.first?.host ?? "Steve Hub"
    }

    public var endpointsSummary: String {
        payload.orderedEndpoints.map(URLNormalizer.canonicalOrigin).joined(separator: "\n")
    }
}

@MainActor
public final class SteveClientCoordinator: ObservableObject {
    @Published public private(set) var summary = HubRuntimeSummary()
    @Published public private(set) var localGatewayURL: URL?
    @Published public private(set) var proxyStartupError: String?
    @Published public private(set) var webReloadNonce = UUID()
    @Published public private(set) var targetHashRoute: String?

    @Published public var isHubSwitcherPresented = false
    @Published public var isPairQRPresented = false
    @Published public var isQRScannerPresented = false
    @Published public var pendingExternalPairConfirmation: PendingPairConfirmation?
    @Published public var externalPairErrorMessage: String?
    @Published public private(set) var lastCreatedPairing: CreatedPairing?

    public let hubStore: HubStore
    public let api: HubClientAPI
    public let resolver: EndpointResolver
    public let proxy: HubLoopbackProxy
    public let clientDescriptor: ClientDescriptor

    private var monitorTask: Task<Void, Never>?
    private var started = false
    private var activationGeneration: UInt64 = 0

    public init(
        hubStore: HubStore? = nil,
        api: HubClientAPI = HubClientAPI(),
        resolver: EndpointResolver = EndpointResolver(),
        webRoot: URL? = nil,
        preferredPortDefaults: UserDefaults? = .standard,
        clientDescriptor: ClientDescriptor
    ) {
        self.hubStore = hubStore ?? HubStore()
        self.api = api
        self.resolver = resolver
        self.proxy = HubLoopbackProxy(webRoot: webRoot, preferredPortDefaults: preferredPortDefaults)
        self.clientDescriptor = clientDescriptor

        self.proxy.setAuthEventHandler { [weak self] hubID, event in
            Task { @MainActor [weak self] in
                guard let self else { return }
                switch event {
                case .signedIn(let signed):
                    self.hubStore.updateAuthMetadata(
                        id: hubID,
                        clientID: signed.client_id,
                        userName: signed.user.name,
                        credential: signed.credential
                    )
                    self.hubStore.updateConnectionMetadata(id: hubID, version: signed.hub.version)
                    if self.hubStore.activeHubID == hubID {
                        await self.refreshActiveHubState()
                    }
                case .signedOut:
                    self.hubStore.updateAuthMetadata(id: hubID, clientID: nil, userName: nil, credential: nil)
                    if self.hubStore.activeHubID == hubID {
                        await self.refreshActiveHubState()
                    }
                }
            }
        }
    }

    public func startIfNeeded() {
        guard !started else { return }
        started = true
        Task { [weak self] in
            guard let self else { return }
            await self.ensureGatewayRunning()
            await self.activateCurrentHub(forceReloadWeb: true)
            self.startBackgroundPolling()
        }
    }

    public func handleAppDidBecomeActive() {
        guard started else {
            startIfNeeded()
            return
        }
        Task { [weak self] in
            guard let self else { return }
            let previousURL = self.localGatewayURL
            await self.ensureGatewayRunning()
            let gatewayChanged = (self.localGatewayURL != previousURL)
            await self.activateCurrentHub(forceReloadWeb: gatewayChanged)
        }
    }

    public func retryGatewayStartup() {
        Task { [weak self] in
            guard let self else { return }
            await self.ensureGatewayRunning()
            await self.activateCurrentHub(forceReloadWeb: true)
        }
    }

    private func ensureGatewayRunning() async {
        do {
            let url = try await proxy.ensureRunning()
            localGatewayURL = url
            proxyStartupError = nil
        } catch {
            localGatewayURL = nil
            proxyStartupError = error.localizedDescription
            summary = HubRuntimeSummary(
                phase: .offline,
                errorMessage: error.localizedDescription,
                updatedAt: Date()
            )
        }
    }

    public func stop() {
        activationGeneration &+= 1
        monitorTask?.cancel()
        monitorTask = nil
        proxy.stop()
        localGatewayURL = nil
        started = false
    }

    public func switchToHub(id: String) {
        guard hubStore.activeHubID != id else { return }
        hubStore.selectHub(id: id)
        Task { [weak self] in
            await self?.activateCurrentHub(forceReloadWeb: true)
        }
    }

    public func removeHub(id: String) {
        guard let removed = hubStore.removeHub(id: id) else { return }
        let removedID = removed.profile.id
        let prefix = HubLoopbackProxy.hubCookiePrefix(for: removedID)
        let cookieStore = WKWebsiteDataStore.default().httpCookieStore
        cookieStore.getAllCookies { cookies in
            for cookie in cookies where cookie.name.hasPrefix(prefix) {
                cookieStore.delete(cookie, completionHandler: nil)
            }
        }
        if let cred = removed.credential, !cred.isEmpty,
           let endpoint = removed.profile.orderedEndpoints.first {
            let clientAPI = self.api
            Task {
                await clientAPI.signOutClient(endpoint: endpoint, credential: cred)
            }
        }
        if removed.wasActive {
            Task { [weak self] in
                await self?.activateCurrentHub(forceReloadWeb: true)
            }
        }
    }

    public func reconnectActiveHub() {
        Task { [weak self] in
            guard let self else { return }
            if self.localGatewayURL == nil {
                await self.ensureGatewayRunning()
            }
            await self.activateCurrentHub(forceReloadWeb: true)
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

    // MARK: - External `steve://pair` Confirmation Gate

    public func requestExternalPairingConfirmation(uri: String) {
        let trimmed = uri.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let payload = PairingPayload.parse(trimmed) else {
            externalPairErrorMessage = L10n.tr(
                "配对链接无效，请检查二维码或配对地址格式。",
                "Invalid pairing link. Check the QR code or pairing URI."
            )
            return
        }
        pendingExternalPairConfirmation = PendingPairConfirmation(rawURI: trimmed, payload: payload)
    }

    /// Validates a scanned or pasted `steve://pair` URI from the iOS scanner, dismisses the scanner,
    /// and stages user confirmation so no code is ever claimed before the user approves the target Hub.
    @discardableResult
    public func stageScannedPairingCode(_ raw: String) -> Bool {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let payload = PairingPayload.parse(trimmed) else {
            return false
        }
        isQRScannerPresented = false
        pendingExternalPairConfirmation = PendingPairConfirmation(rawURI: trimmed, payload: payload)
        return true
    }

    public func cancelPendingExternalPairing() {
        pendingExternalPairConfirmation = nil
    }

    @discardableResult
    public func confirmPendingExternalPairing() async -> HubProfile? {
        guard let pending = pendingExternalPairConfirmation else { return nil }
        pendingExternalPairConfirmation = nil
        do {
            return try await pairWithQRCode(uri: pending.rawURI)
        } catch {
            externalPairErrorMessage = error.localizedDescription
            return nil
        }
    }

    /// Pairs this device by resolving the QR payload's public-first / LAN-fallback endpoints
    /// and claiming the single-use `stp_...` code for a dedicated Keychain-stored client credential.
    /// Private so every external/UI entry point must pass through `pendingExternalPairConfirmation`.
    @discardableResult
    private func pairWithQRCode(uri: String) async throws -> HubProfile {
        guard let payload = PairingPayload.parse(uri) else {
            throw HubClientAPIError.invalidResponse
        }
        let resolved = try await resolver.resolve(pairing: payload)
        let claim = try await api.claimPairing(
            endpoint: resolved.url,
            code: payload.code,
            client: clientDescriptor
        )
        guard let credential = claim.credential, !credential.isEmpty else {
            throw HubClientAPIError.missingCredential
        }

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
        let saved = try hubStore.addPairedProfile(profile, credential: credential, makeActive: true)
        await activateCurrentHub(forceReloadWeb: true)
        return saved
    }

    public func signInActiveHubWithToken(user: String, token: String) async throws {
        guard let profile = hubStore.currentProfile else {
            throw EndpointResolutionError.noCandidates
        }
        let resolved = try await resolver.resolve(profile: profile)
        let signed = try await api.signInWithToken(
            endpoint: resolved.url,
            user: user,
            token: token,
            client: clientDescriptor
        )
        guard let credential = signed.credential, !credential.isEmpty else {
            throw HubClientAPIError.missingCredential
        }
        guard hubStore.updateAuthMetadata(
            id: profile.id,
            clientID: signed.client_id,
            userName: signed.user.name,
            credential: credential
        ) else {
            throw CredentialStoreError.saveFailed
        }
        hubStore.updateConnectionMetadata(id: profile.id, version: signed.hub.version)
        await activateCurrentHub(forceReloadWeb: true)
    }

    public func createPairingForActiveHub() async throws -> CreatedPairing {
        guard let profile = hubStore.currentProfile else {
            throw EndpointResolutionError.noCandidates
        }
        let endpoint = summary.activeEndpoint ?? profile.orderedEndpoints.first
        guard let endpoint else {
            throw EndpointResolutionError.noCandidates
        }
        let created = try await api.createPairing(
            endpoint: endpoint,
            credential: hubStore.credential(for: profile.id)
        )
        lastCreatedPairing = created
        return created
    }

    public func pollPairingStatus(id: String) async throws -> PairingClaimStatus {
        guard let profile = hubStore.currentProfile else {
            throw EndpointResolutionError.noCandidates
        }
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

    public func refreshActiveHubState() async {
        await activateCurrentHub(forceReloadWeb: false)
    }

    public func activateCurrentHub(forceReloadWeb: Bool) async {
        if forceReloadWeb {
            activationGeneration &+= 1
        }
        let capturedGen = activationGeneration

        guard let profile = hubStore.currentProfile else {
            proxy.updateTarget(.init(hubID: "", baseURL: nil, credential: nil))
            summary = HubRuntimeSummary(
                phase: .offline,
                activeEndpoint: nil,
                route: .unknown,
                updatedAt: Date()
            )
            if forceReloadWeb {
                webReloadNonce = UUID()
            }
            return
        }

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
            guard activationGeneration == capturedGen, hubStore.activeHubID == profile.id else {
                return
            }
            proxy.updateTarget(.init(hubID: profile.id, baseURL: resolved.url, credential: credential))
            hubStore.updateConnectionMetadata(id: profile.id, version: resolved.hello.version)

            let liveSummary = try? await api.fetchStateSummary(
                endpoint: resolved.url,
                route: resolved.route,
                helloVersion: resolved.hello.version,
                credential: credential
            )
            guard activationGeneration == capturedGen, hubStore.activeHubID == profile.id else {
                return
            }
            if let liveSummary {
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
            guard activationGeneration == capturedGen, hubStore.activeHubID == profile.id else {
                return
            }
            // Even when offline, point the proxy at the primary candidate so user retries reach it once up
            proxy.updateTarget(.init(hubID: profile.id, baseURL: profile.orderedEndpoints.first, credential: credential))
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
