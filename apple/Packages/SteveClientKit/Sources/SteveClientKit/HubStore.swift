import Foundation

@MainActor
public final class HubStore: ObservableObject {
    @Published public private(set) var profiles: [HubProfile]
    @Published public private(set) var activeHubID: String

    private let defaults: UserDefaults
    private let profilesKey: String
    private let activeKey: String
    private let includeLocalDefault: Bool
    private let fallbackDefaultProfile: HubProfile
    public let credentials: CredentialStoring

    public nonisolated static var defaultIncludeLocalDefault: Bool {
        #if os(iOS)
        return false
        #else
        return true
        #endif
    }

    public init(
        defaults: UserDefaults = .standard,
        profilesKey: String = "steve.apple.hubs",
        activeKey: String = "steve.apple.activeHubID",
        credentials: CredentialStoring = KeychainCredentialStore(),
        defaultProfile: HubProfile = LocalHubDiscovery.defaultProfile(),
        includeLocalDefault: Bool = HubStore.defaultIncludeLocalDefault
    ) {
        self.defaults = defaults
        self.profilesKey = profilesKey
        self.activeKey = activeKey
        self.credentials = credentials
        self.includeLocalDefault = includeLocalDefault
        self.fallbackDefaultProfile = defaultProfile

        var loaded: [HubProfile] = []
        if let data = defaults.data(forKey: profilesKey),
           let decoded = try? JSONDecoder().decode([HubProfile].self, from: data) {
            loaded = decoded
        }

        if includeLocalDefault {
            if let idx = loaded.firstIndex(where: { $0.id == HubProfile.localDefaultID || $0.isLocalDefault }) {
                let existing = loaded[idx]
                let onlyLoopback = existing.publicURL == nil
                    && existing.lanURLs.allSatisfy { HubRouteKind.classify(url: $0) == .loopback }
                if (existing.lanURLs.isEmpty && existing.publicURL == nil) || onlyLoopback {
                    let oldSig = Self.endpointSignature(of: existing)
                    let newSig = Self.endpointSignature(of: defaultProfile)
                    loaded[idx].publicURL = defaultProfile.publicURL
                    loaded[idx].lanURLs = defaultProfile.lanURLs
                    if oldSig != newSig {
                        credentials.setCredential(nil, for: existing.id)
                        loaded[idx].clientID = nil
                        loaded[idx].userName = nil
                    }
                }
            } else {
                loaded.insert(defaultProfile, at: 0)
            }
        } else {
            loaded.removeAll { $0.isLocalDefault || $0.id == HubProfile.localDefaultID }
        }

        let savedActive = defaults.string(forKey: activeKey)
        if let savedActive, loaded.contains(where: { $0.id == savedActive }) {
            self.activeHubID = savedActive
        } else {
            self.activeHubID = loaded.first?.id ?? ""
        }
        self.profiles = loaded
        persist()
    }

    public var hasProfiles: Bool {
        !profiles.isEmpty
    }

    public var currentProfile: HubProfile? {
        profiles.first(where: { $0.id == activeHubID }) ?? profiles.first
    }

    public var activeProfile: HubProfile {
        currentProfile ?? fallbackDefaultProfile
    }

    public func selectHub(id: String) {
        guard profiles.contains(where: { $0.id == id }) else { return }
        activeHubID = id
        persist()
    }

    /// Saves a QR-paired Hub profile with a mandatory freshly minted credential.
    /// Never merges into `local-default` or into any profile whose endpoint set differs.
    /// Throws `CredentialStoreError.saveFailed` if the credential cannot be persisted.
    @discardableResult
    public func addPairedProfile(_ profile: HubProfile, credential: String, makeActive: Bool = true) throws -> HubProfile {
        var candidate = profile
        candidate.isLocalDefault = false
        if candidate.id == HubProfile.localDefaultID {
            candidate.id = UUID().uuidString
        }

        let candidateSig = Self.endpointSignature(of: candidate)
        let existingIdx = profiles.firstIndex(where: {
            !$0.isLocalDefault && $0.id != HubProfile.localDefaultID && Self.endpointSignature(of: $0) == candidateSig
        })
        if let existingIdx {
            let existing = profiles[existingIdx]
            candidate.id = existing.id
            if candidate.name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                candidate.name = existing.name
            }
        }

        guard credentials.setCredential(credential, for: candidate.id) else {
            throw CredentialStoreError.saveFailed
        }

        if let existingIdx {
            profiles[existingIdx] = candidate
        } else {
            profiles.append(candidate)
        }
        if makeActive {
            activeHubID = candidate.id
        }
        persist()
        return candidate
    }

    @discardableResult
    public func upsertProfile(_ profile: HubProfile, credential: String? = nil, makeActive: Bool = true) -> HubProfile {
        var updated = profile
        if let existingIdx = matchingIndex(for: profile) {
            let existing = profiles[existingIdx]
            let endpointsChanged = Self.endpointSignature(of: existing) != Self.endpointSignature(of: updated)
            updated.id = existing.id
            updated.isLocalDefault = existing.isLocalDefault
            if updated.name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                updated.name = existing.name
            }
            if endpointsChanged && credential == nil {
                credentials.setCredential(nil, for: updated.id)
                updated.clientID = nil
                updated.userName = nil
            } else {
                if updated.clientID == nil { updated.clientID = existing.clientID }
                if updated.userName == nil { updated.userName = existing.userName }
                if updated.lastKnownVersion == nil { updated.lastKnownVersion = existing.lastKnownVersion }
            }
            profiles[existingIdx] = updated
        } else {
            profiles.append(updated)
        }

        if let credential, !credential.isEmpty {
            credentials.setCredential(credential, for: updated.id)
        }
        if makeActive {
            activeHubID = updated.id
        }
        persist()
        return updated
    }

    @discardableResult
    public func addManualHub(address: String, name: String? = nil, makeActive: Bool = true) -> HubProfile? {
        guard let url = URLNormalizer.normalize(address) else { return nil }
        let route = HubRouteKind.classify(url: url)
        let displayName: String
        if let name, !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            displayName = name.trimmingCharacters(in: .whitespacesAndNewlines)
        } else {
            displayName = url.host ?? URLNormalizer.canonicalOrigin(url)
        }
        let profile = HubProfile(
            name: displayName,
            publicURL: route == .publicNetwork ? url : nil,
            lanURLs: route == .publicNetwork ? [] : [url],
            isLocalDefault: false
        )
        return upsertProfile(profile, makeActive: makeActive)
    }

    public func updateConnectionMetadata(id: String, version: String?, connectedAt: Date = Date()) {
        guard let idx = profiles.firstIndex(where: { $0.id == id }) else { return }
        if let version, !version.isEmpty {
            profiles[idx].lastKnownVersion = version
        }
        profiles[idx].lastConnectedAt = connectedAt
        persist()
    }

    @discardableResult
    public func updateAuthMetadata(id: String, clientID: String?, userName: String?, credential: String?) -> Bool {
        guard let idx = profiles.firstIndex(where: { $0.id == id }) else { return false }
        guard credentials.setCredential(credential, for: id) else { return false }
        profiles[idx].clientID = clientID
        profiles[idx].userName = userName
        persist()
        return true
    }

    public struct RemovedHub: Sendable {
        public let profile: HubProfile
        public let credential: String?
        public let wasActive: Bool
    }

    @discardableResult
    public func removeHub(id: String) -> RemovedHub? {
        guard let idx = profiles.firstIndex(where: { $0.id == id }),
              !profiles[idx].isLocalDefault else {
            return nil
        }
        let removedProfile = profiles[idx]
        let oldCred = credentials.credential(for: id)
        let wasActive = (activeHubID == id)
        credentials.setCredential(nil, for: id)
        profiles.remove(at: idx)
        if wasActive {
            activeHubID = profiles.first?.id ?? ""
        }
        persist()
        return RemovedHub(profile: removedProfile, credential: oldCred, wasActive: wasActive)
    }

    public func credential(for id: String) -> String? {
        credentials.credential(for: id)
    }

    private struct EndpointSignature: Equatable {
        let publicOrigin: String?
        let lanOrigins: Set<String>
    }

    private static func endpointSignature(of profile: HubProfile) -> EndpointSignature {
        EndpointSignature(
            publicOrigin: profile.publicURL.map(URLNormalizer.canonicalOrigin),
            lanOrigins: Set(profile.lanURLs.map(URLNormalizer.canonicalOrigin))
        )
    }

    private func matchingIndex(for candidate: HubProfile) -> Int? {
        if let byID = profiles.firstIndex(where: { $0.id == candidate.id }) {
            return byID
        }
        guard !candidate.isLocalDefault else { return nil }
        let candidateSig = Self.endpointSignature(of: candidate)
        guard candidateSig.publicOrigin != nil || !candidateSig.lanOrigins.isEmpty else { return nil }
        return profiles.firstIndex { existing in
            !existing.isLocalDefault
                && existing.id != HubProfile.localDefaultID
                && Self.endpointSignature(of: existing) == candidateSig
        }
    }

    private func persist() {
        if let encoded = try? JSONEncoder().encode(profiles) {
            defaults.set(encoded, forKey: profilesKey)
        }
        defaults.set(activeHubID, forKey: activeKey)
    }
}
