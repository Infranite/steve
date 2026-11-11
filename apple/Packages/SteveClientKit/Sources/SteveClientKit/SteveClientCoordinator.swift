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

public enum BridgeCallError: Error, LocalizedError, Sendable, Equatable {
    case invalidArgument(String)
    case unsupportedOperation(String)
    case operationFailed(String)

    public var errorDescription: String? {
        switch self {
        case .invalidArgument(let message), .operationFailed(let message):
            return message
        case .unsupportedOperation(let op):
            return L10n.tr("不支持的原生操作：\(op)", "Unsupported native bridge operation: \(op)")
        }
    }
}

@MainActor
public final class SteveClientCoordinator: ObservableObject {
    @Published public private(set) var summary = HubRuntimeSummary()
    @Published public private(set) var localGatewayURL: URL?
    @Published public private(set) var proxyStartupError: String?
    @Published public private(set) var webReloadNonce = UUID()
    @Published public private(set) var targetHashRoute: String?

    @Published public var isQRScannerPresented = false
    @Published public private(set) var pendingExternalPairConfirmation: PendingPairConfirmation?
    @Published public private(set) var isPairConfirmationPresented = false
    @Published public var externalPairErrorMessage: String?

    public let hubStore: HubStore
    public let api: HubClientAPI
    public let resolver: EndpointResolver
    public let proxy: HubLoopbackProxy
    public let clientDescriptor: ClientDescriptor

    private var monitorTask: Task<Void, Never>?
    private var started = false
    private var activationGeneration: UInt64 = 0
    /// A forced activation owes the WebView a reload. If that activation is superseded before it
    /// reaches the reload, the activation that supersedes it pays the debt, so the window is never
    /// left on the previous Hub's page with a retired gateway key.
    private var webReloadOwed = false
    private var pendingPairContinuation: CheckedContinuation<Void, Error>?
    private var confirmingPairID: UUID?
    private let purgeWebCookiesHandler: ((Set<String>) -> Void)?

    public init(
        hubStore: HubStore? = nil,
        api: HubClientAPI = HubClientAPI(),
        resolver: EndpointResolver = EndpointResolver(),
        webRoot: URL? = nil,
        preferredPortDefaults: UserDefaults? = .standard,
        clientDescriptor: ClientDescriptor,
        purgeWebCookiesHandler: ((Set<String>) -> Void)? = nil
    ) {
        self.hubStore = hubStore ?? HubStore()
        self.api = api
        self.resolver = resolver
        self.proxy = HubLoopbackProxy(webRoot: webRoot, preferredPortDefaults: preferredPortDefaults)
        self.clientDescriptor = clientDescriptor
        self.purgeWebCookiesHandler = purgeWebCookiesHandler

        self.proxy.setAuthEventHandler { [weak self] hubID, event in
            Task { @MainActor [weak self] in
                guard let self else { return }
                // Only the active Hub's sign-in/out makes an in-flight activation stale (it captured the
                // old credential). A late answer from a Hub that is no longer active must not cancel the
                // activation of the Hub being switched to.
                if self.hubStore.activeHubID == hubID {
                    self.activationGeneration &+= 1
                }
                switch event {
                case .signedIn(let signed):
                    let previousCredential = self.hubStore.credential(for: hubID)
                    self.hubStore.updateAuthMetadata(
                        id: hubID,
                        clientID: signed.client_id,
                        userName: signed.user.name,
                        credential: signed.credential
                    )
                    self.hubStore.updateConnectionMetadata(id: hubID, version: signed.hub.version)
                    if let previousCredential,
                       !previousCredential.isEmpty,
                       previousCredential != signed.credential,
                       let endpoint = self.endpointForHub(id: hubID) {
                        let clientAPI = self.api
                        Task {
                            await clientAPI.signOutClient(endpoint: endpoint, credential: previousCredential)
                        }
                    }
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

    private func endpointForHub(id: String) -> URL? {
        if hubStore.activeHubID == id, let active = summary.activeEndpoint {
            return active
        }
        return hubStore.profiles.first(where: { $0.id == id })?.orderedEndpoints.first
    }

    public func startIfNeeded() {
        guard !started else { return }
        started = true
        Task { [weak self] in
            guard let self else { return }
            do {
                let url = try await self.proxy.ensureRunning()
                self.proxyStartupError = nil
                await self.activateCurrentHub(forceReloadWeb: false)
                self.localGatewayURL = url
            } catch {
                self.localGatewayURL = nil
                self.proxyStartupError = error.localizedDescription
                self.summary = HubRuntimeSummary(
                    phase: .offline,
                    errorMessage: error.localizedDescription,
                    updatedAt: Date()
                )
            }
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

    public func failGatewayIsolation(_ message: String) {
        proxy.stop()
        localGatewayURL = nil
        proxyStartupError = message
        summary = HubRuntimeSummary(
            phase: .offline,
            errorMessage: message,
            updatedAt: Date()
        )
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
        if let cont = pendingPairContinuation {
            pendingPairContinuation = nil
            cont.resume(throwing: BridgeCallError.operationFailed(L10n.tr("客户端已停止", "Client stopped")))
        }
        pendingExternalPairConfirmation = nil
        isPairConfirmationPresented = false
        confirmingPairID = nil
        monitorTask?.cancel()
        monitorTask = nil
        proxy.stop()
        localGatewayURL = nil
        started = false
    }

    // MARK: - Native Bridge v2 (`window.__STEVE_NATIVE__`)

    public static func bridgeCapabilities(for platform: String) -> [String] {
        var caps = ["hubs.list", "hubs.switch", "hubs.add", "hubs.forget", "pair.claim"]
        if platform == "ios" {
            caps.append("pair.scan")
        }
        return caps
    }

    public func listNativeHubs() -> [NativeHubBridgeItem] {
        let currentID = hubStore.activeHubID
        let proxyTarget = proxy.currentTarget()
        return hubStore.profiles.map { profile in
            let isCurrent = (profile.id == currentID)
            let hasStoredCredential = !(hubStore.credential(for: profile.id) ?? "").isEmpty
            let signedIn: Bool
            if isCurrent && summary.phase == .authRequired {
                signedIn = false
            } else if isCurrent && summary.phase == .connected {
                signedIn = true
            } else {
                signedIn = hasStoredCredential
            }

            let route: String?
            if isCurrent && (summary.phase == .connected || summary.phase == .authRequired) {
                route = summary.route.bridgeRouteValue
            } else {
                route = nil
            }

            let address: String
            if isCurrent,
               (summary.phase == .connected || summary.phase == .authRequired),
               let inUse = summary.activeEndpoint ?? (proxyTarget.hubID == profile.id ? proxyTarget.baseURL : nil) {
                address = URLNormalizer.canonicalOrigin(inUse)
            } else {
                address = profile.primaryDisplayAddress
            }

            let version = (isCurrent ? summary.hubVersion : nil) ?? profile.lastKnownVersion
            return NativeHubBridgeItem(
                id: profile.id,
                name: profile.name,
                address: address,
                current: isCurrent,
                signedIn: signedIn,
                route: route,
                version: version,
                removable: !profile.isLocalDefault
            )
        }
    }

    public func handleBridgeCall(
        op: String,
        args: [String: Any],
        platform: String
    ) async throws -> [String: Any] {
        switch op {
        case "hubs.list":
            return ["hubs": listNativeHubs().map { $0.asBridgeDictionary() }]

        case "hubs.switch":
            guard let id = (args["id"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !id.isEmpty,
                  hubStore.profiles.contains(where: { $0.id == id }) else {
                throw BridgeCallError.invalidArgument(L10n.tr("未找到指定的 Hub", "Unknown Hub"))
            }
            let wasAlreadyActive = (hubStore.activeHubID == id)
            if !wasAlreadyActive {
                hubStore.selectHub(id: id)
                await activateCurrentHub(forceReloadWeb: true)
                return [:]
            }
            // Retrying an offline active Hub from HubChooser (`down = true`): reload if recovered, else surface error.
            activationGeneration &+= 1
            await activateCurrentHub(forceReloadWeb: false)
            if summary.phase == .offline {
                throw BridgeCallError.operationFailed(
                    summary.errorMessage ?? L10n.tr("无法连接到该 Hub", "Could not connect to Hub")
                )
            }
            webReloadNonce = UUID()
            return [:]

        case "hubs.add":
            let rawAddress = (args["address"] as? String) ?? ""
            let rawName = args["name"] as? String
            guard var candidate = HubStore.makeManualCandidate(address: rawAddress, name: rawName) else {
                throw BridgeCallError.invalidArgument(L10n.tr("Hub 地址格式无效", "Invalid Hub address"))
            }
            let resolved: ResolvedHubEndpoint
            do {
                resolved = try await resolver.resolve(profile: candidate)
            } catch let err as EndpointResolutionError {
                if case .notSteveHub(let webURL) = err,
                   let behindURL = await EndpointResolver.resolveHubBehind(webURL: webURL),
                   let behindCandidate = HubStore.makeManualCandidate(address: behindURL.absoluteString, name: rawName) {
                    candidate = behindCandidate
                    resolved = try await resolver.resolve(profile: candidate)
                } else {
                    throw err
                }
            }
            var verified = candidate
            verified.lastKnownVersion = resolved.hello.version
            verified.lastConnectedAt = Date()
            _ = hubStore.upsertProfile(verified, makeActive: true)
            purgePendingWebCookies()
            await activateCurrentHub(forceReloadWeb: true)
            return [:]

        case "hubs.forget":
            guard let id = (args["id"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !id.isEmpty,
                  let target = hubStore.profiles.first(where: { $0.id == id }) else {
                throw BridgeCallError.invalidArgument(L10n.tr("未找到指定的 Hub", "Unknown Hub"))
            }
            guard !target.isLocalDefault else {
                throw BridgeCallError.invalidArgument(
                    L10n.tr("默认本机 Hub 不可移除", "The default local Hub cannot be removed")
                )
            }
            await forgetHub(id: id)
            return [:]

        case "pair.claim":
            let rawLink = (args["link"] as? String)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            guard let payload = PairingPayload.parse(rawLink) else {
                throw BridgeCallError.invalidArgument(
                    L10n.tr(
                        "配对链接无效，请检查二维码或配对地址格式。",
                        "Invalid pairing link. Check the QR code or pairing link."
                    )
                )
            }
            try await requestBridgePairingConfirmation(rawURI: rawLink, payload: payload)
            return [:]

        case "pair.scan":
            guard platform == "ios" else {
                throw BridgeCallError.unsupportedOperation(op)
            }
            isQRScannerPresented = true
            return [:]

        default:
            throw BridgeCallError.unsupportedOperation(op)
        }
    }

    // MARK: - Native Actions (Menu Bar / Commands / Deep Links / Scanner)

    public func switchToHub(id: String) {
        guard hubStore.profiles.contains(where: { $0.id == id }) else { return }
        guard hubStore.activeHubID != id else { return }
        hubStore.selectHub(id: id)
        Task { [weak self] in
            await self?.activateCurrentHub(forceReloadWeb: true)
        }
    }

    public func removeHub(id: String) {
        Task { [weak self] in
            await self?.forgetHub(id: id)
        }
    }

    private func forgetHub(id: String) async {
        guard let removed = hubStore.removeHub(id: id) else { return }
        if removed.wasActive {
            activationGeneration &+= 1
        }
        purgePendingWebCookies()
        if let cred = removed.credential, !cred.isEmpty,
           let endpoint = removed.profile.orderedEndpoints.first {
            let clientAPI = self.api
            Task {
                await clientAPI.signOutClient(endpoint: endpoint, credential: cred)
            }
        }
        if removed.wasActive {
            await activateCurrentHub(forceReloadWeb: true)
        }
    }

    public func purgePendingWebCookies() {
        let purgedIDs = hubStore.drainCookieCleanupHubIDs()
        guard !purgedIDs.isEmpty else { return }
        if let purgeWebCookiesHandler {
            purgeWebCookiesHandler(purgedIDs)
            return
        }
        let cookieStore = WKWebsiteDataStore.default().httpCookieStore
        cookieStore.getAllCookies { cookies in
            for cookie in cookies where HubLoopbackProxy.shouldDeleteWebCookie(named: cookie.name, purgedHubIDs: purgedIDs) {
                cookieStore.delete(cookie, completionHandler: nil)
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

    // MARK: - Pairing Confirmation Gate (Unified Across Bridge, Scanner & `steve://`)

    /// Title of the native confirmation. The apps use it for the alert and to recognise that alert on screen.
    public static var pairConfirmationTitle: String {
        L10n.tr("确认配对并连接到此 Hub？", "Pair and connect to this Hub?")
    }

    /// Installed by the app: is the confirmation alert on screen right now? `isPairConfirmationPresented`
    /// only says the alert was asked for; the system can decline to show it (no window, another
    /// presentation in the way). Without a probe the flag is all there is to go by.
    public var pairConfirmationVisibilityProbe: (@MainActor () -> Bool)?

    private enum PairingRefusal {
        case confirmationOnScreen
        case claimInProgress
    }

    /// A new request is refused only while a confirmation is really on screen or a confirmed code is
    /// being claimed. A pending item nobody can see never blocks: the new request replaces it.
    private var pairingRefusal: PairingRefusal? {
        if confirmingPairID != nil {
            return .claimInProgress
        }
        if pendingExternalPairConfirmation != nil,
           isPairConfirmationPresented,
           pairConfirmationVisibilityProbe?() ?? true {
            return .confirmationOnScreen
        }
        return nil
    }

    /// Why a pairing request would be refused right now, or nil when one can be staged.
    public var pairingRefusalMessage: String? {
        pairingRefusal == nil ? nil : Self.busyPairingMessage
    }

    private static var busyPairingMessage: String {
        L10n.tr(
            "已有正在处理的配对请求，请先完成或取消当前提示。",
            "Another pairing confirmation is already in progress."
        )
    }

    private static var supersededPairingMessage: String {
        L10n.tr(
            "此配对请求未能显示确认提示，已被新的配对请求取代，请重试。",
            "This pairing request was never confirmed and a newer one replaced it. Try again."
        )
    }

    /// Refusal for the entry points that have no caller to answer (deep link, scanner). While a claim
    /// is running no confirmation is up, so the message shows now. While a confirmation is on screen
    /// that alert is the answer: nothing is stored to pop up after it as a failure of the wrong pairing.
    private func noteRefusedPairingRequest(_ refusal: PairingRefusal) {
        if refusal == .claimInProgress {
            externalPairErrorMessage = Self.busyPairingMessage
        }
    }

    /// Makes `rawURI` the one pending confirmation. An earlier item that is still pending was not on
    /// screen (the callers checked `pairingRefusal`); its `pair.claim` is rejected with a showable error.
    private func stagePairConfirmation(
        rawURI: String,
        payload: PairingPayload,
        continuation: CheckedContinuation<Void, Error>?
    ) {
        if let superseded = pendingPairContinuation {
            pendingPairContinuation = nil
            superseded.resume(throwing: BridgeCallError.operationFailed(Self.supersededPairingMessage))
        }
        externalPairErrorMessage = nil
        pendingPairContinuation = continuation
        pendingExternalPairConfirmation = PendingPairConfirmation(rawURI: rawURI, payload: payload)
        isPairConfirmationPresented = true
    }

    public func requestExternalPairingConfirmation(uri: String) {
        if let refusal = pairingRefusal {
            noteRefusedPairingRequest(refusal)
            return
        }
        let trimmed = uri.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let payload = PairingPayload.parse(trimmed) else {
            externalPairErrorMessage = L10n.tr(
                "配对链接无效，请检查二维码或配对地址格式。",
                "Invalid pairing link. Check the QR code or pairing link."
            )
            return
        }
        stagePairConfirmation(rawURI: trimmed, payload: payload, continuation: nil)
    }

    /// Validates a scanned or pasted pairing link from the iOS scanner, dismisses the scanner,
    /// and stages user confirmation so no code is ever claimed before the user approves the target Hub.
    /// Returns false for a link that does not parse (the scanner keeps scanning) and for a refusal;
    /// a refusal during a claim leaves its message in `externalPairErrorMessage`.
    @discardableResult
    public func stageScannedPairingCode(_ raw: String) -> Bool {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let payload = PairingPayload.parse(trimmed) else {
            return false
        }
        if let refusal = pairingRefusal {
            noteRefusedPairingRequest(refusal)
            return false
        }
        isQRScannerPresented = false
        stagePairConfirmation(rawURI: trimmed, payload: payload, continuation: nil)
        return true
    }

    private func requestBridgePairingConfirmation(rawURI: String, payload: PairingPayload) async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            guard self.pairingRefusal == nil else {
                continuation.resume(throwing: BridgeCallError.operationFailed(Self.busyPairingMessage))
                return
            }
            self.stagePairConfirmation(rawURI: rawURI, payload: payload, continuation: continuation)
        }
    }

    /// The alert's `isPresented` setter. It hides the alert and nothing else: it cancels nothing and
    /// schedules nothing, so it does not matter whether SwiftUI calls it before or after a button's
    /// action. The item stays pending until a button answers by id (`cancelPendingExternalPairing(id:)`,
    /// `beginConfirmExternalPairing(id:)`) or a new request replaces it.
    public func hidePairConfirmationAlert() {
        isPairConfirmationPresented = false
    }

    /// The Cancel button (also Escape and a system dismissal, which trigger the `.cancel` role).
    /// `pair.claim` resolves `{}`: the person declined.
    public func cancelPendingExternalPairing(id: UUID? = nil) {
        guard let current = pendingExternalPairConfirmation else { return }
        if let id, current.id != id {
            return
        }
        pendingExternalPairConfirmation = nil
        isPairConfirmationPresented = false
        if let cont = pendingPairContinuation {
            pendingPairContinuation = nil
            cont.resume(returning: ())
        }
    }

    private struct TakenPairConfirmation {
        let confirmation: PendingPairConfirmation
        let continuation: CheckedContinuation<Void, Error>?
    }

    private func takePendingConfirmationForClaim(id: UUID) -> TakenPairConfirmation? {
        guard confirmingPairID == nil,
              let current = pendingExternalPairConfirmation,
              current.id == id else {
            return nil
        }
        let cont = pendingPairContinuation
        pendingPairContinuation = nil
        pendingExternalPairConfirmation = nil
        isPairConfirmationPresented = false
        externalPairErrorMessage = nil
        confirmingPairID = id
        return TakenPairConfirmation(confirmation: current, continuation: cont)
    }

    @discardableResult
    private func completeTakenExternalPairing(_ taken: TakenPairConfirmation) async -> HubProfile? {
        defer {
            if confirmingPairID == taken.confirmation.id {
                confirmingPairID = nil
            }
        }
        do {
            let profile = try await pairWithLink(uri: taken.confirmation.rawURI)
            taken.continuation?.resume(returning: ())
            return profile
        } catch {
            if let cont = taken.continuation {
                cont.resume(throwing: error)
            } else {
                externalPairErrorMessage = error.localizedDescription
            }
            return nil
        }
    }

    /// Synchronously claims ownership of `id` in the alert button action before launching the async Hub claim.
    public func beginConfirmExternalPairing(id: UUID) {
        guard let taken = takePendingConfirmationForClaim(id: id) else { return }
        Task { @MainActor [weak self] in
            guard let self else {
                taken.continuation?.resume(
                    throwing: BridgeCallError.operationFailed(L10n.tr("客户端已停止", "Client stopped"))
                )
                return
            }
            _ = await self.completeTakenExternalPairing(taken)
        }
    }

    @discardableResult
    public func confirmPendingExternalPairing(id: UUID) async -> HubProfile? {
        guard let taken = takePendingConfirmationForClaim(id: id) else { return nil }
        return await completeTakenExternalPairing(taken)
    }

    /// Pairs this device by resolving the pairing link's public-first / LAN-fallback endpoints
    /// and claiming the single-use `stp_...` code for a dedicated Keychain-stored client credential.
    /// Private so every external/UI/bridge entry point must pass through `pendingExternalPairConfirmation`.
    @discardableResult
    private func pairWithLink(uri: String) async throws -> HubProfile {
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

        var existingCredentialsByID: [String: String] = [:]
        for p in hubStore.profiles {
            if let c = hubStore.credential(for: p.id), !c.isEmpty {
                existingCredentialsByID[p.id] = c
            }
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
        activationGeneration &+= 1
        let saved = try hubStore.addPairedProfile(profile, credential: credential, makeActive: true)
        if let replacedCred = existingCredentialsByID[saved.id], replacedCred != credential {
            let clientAPI = self.api
            let endpoint = resolved.url
            Task {
                await clientAPI.signOutClient(endpoint: endpoint, credential: replacedCred)
            }
        }
        purgePendingWebCookies()
        await activateCurrentHub(forceReloadWeb: true)
        return saved
    }

    // MARK: - Internal Resolution & Status Polling

    public func refreshActiveHubState() async {
        await activateCurrentHub(forceReloadWeb: false)
    }

    public func activateCurrentHub(forceReloadWeb: Bool) async {
        purgePendingWebCookies()
        let previousTargetHubID = proxy.currentTarget().hubID
        if forceReloadWeb {
            activationGeneration &+= 1
            webReloadOwed = true
        }
        let capturedGen = activationGeneration

        guard let profile = hubStore.currentProfile else {
            if forceReloadWeb || !previousTargetHubID.isEmpty {
                proxy.rotateAuthToken()
            }
            proxy.updateTarget(.init(hubID: "", baseURL: nil, credential: nil))
            summary = HubRuntimeSummary(
                phase: .offline,
                activeEndpoint: nil,
                route: .unknown,
                updatedAt: Date()
            )
            issueOwedWebReload()
            return
        }

        let hubChanged = !previousTargetHubID.isEmpty && previousTargetHubID != profile.id
        if forceReloadWeb {
            if hubChanged {
                // Immediately invalidate the previous Hub page's gateway token and sever upstream forwarding
                // so the old Hub's open page cannot talk to either Hub while resolving or reloading.
                proxy.rotateAuthToken()
                proxy.updateTarget(.init(hubID: profile.id, baseURL: nil, credential: nil))
            }
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
            let currentProxyHubID = proxy.currentTarget().hubID
            if !currentProxyHubID.isEmpty && currentProxyHubID != profile.id {
                proxy.rotateAuthToken()
            }
            let freshCredential = hubStore.credential(for: profile.id)
            proxy.updateTarget(.init(hubID: profile.id, baseURL: resolved.url, credential: freshCredential))
            hubStore.updateConnectionMetadata(id: profile.id, version: resolved.hello.version)

            let hasCred = !(freshCredential ?? "").isEmpty
            summary = HubRuntimeSummary(
                phase: hasCred ? .connected : .authRequired,
                activeEndpoint: resolved.url,
                route: resolved.route,
                hubVersion: resolved.hello.version,
                updatedAt: Date()
            )

            // Trigger the WebView reload immediately when the proxy target changes — before `fetchStateSummary` —
            // so the old Hub's page is never left running against the newly selected Hub.
            issueOwedWebReload()

            let liveSummary = try? await api.fetchStateSummary(
                endpoint: resolved.url,
                route: resolved.route,
                helloVersion: resolved.hello.version,
                credential: freshCredential
            )
            guard activationGeneration == capturedGen, hubStore.activeHubID == profile.id else {
                return
            }
            let latestCredential = hubStore.credential(for: profile.id)
            if latestCredential != freshCredential {
                proxy.updateTarget(.init(hubID: profile.id, baseURL: resolved.url, credential: latestCredential))
            }
            if let liveSummary {
                summary = liveSummary
            }
        } catch {
            guard activationGeneration == capturedGen, hubStore.activeHubID == profile.id else {
                return
            }
            let currentProxyHubID = proxy.currentTarget().hubID
            if !currentProxyHubID.isEmpty && currentProxyHubID != profile.id {
                proxy.rotateAuthToken()
            }
            let freshCredential = hubStore.credential(for: profile.id)
            // Leave baseURL nil when unreachable so `/auth/hello` returns an immediate 503 to the web UI.
            proxy.updateTarget(.init(hubID: profile.id, baseURL: nil, credential: freshCredential))
            summary = HubRuntimeSummary(
                phase: .offline,
                activeEndpoint: nil,
                route: profile.orderedEndpoints.first.map { HubRouteKind.classify(url: $0, explicitPublicURL: profile.publicURL) } ?? .unknown,
                hubVersion: profile.lastKnownVersion,
                errorMessage: error.localizedDescription,
                updatedAt: Date()
            )
            issueOwedWebReload()
        }
    }

    private func issueOwedWebReload() {
        guard webReloadOwed else { return }
        webReloadOwed = false
        webReloadNonce = UUID()
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
