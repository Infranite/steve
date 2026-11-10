import Foundation

@MainActor
public final class HubStore: ObservableObject {
    @Published public private(set) var profiles: [HubProfile]
    @Published public private(set) var activeHubID: String

    private let defaults: UserDefaults
    private let profilesKey: String
    private let activeKey: String
    public let credentials: CredentialStoring

    public init(
        defaults: UserDefaults = .standard,
        profilesKey: String = "steve.apple.hubs",
        activeKey: String = "steve.apple.activeHubID",
        credentials: CredentialStoring = KeychainCredentialStore(),
        defaultProfile: HubProfile = LocalHubDiscovery.defaultProfile()
    ) {
        self.defaults = defaults
        self.profilesKey = profilesKey
        self.activeKey = activeKey
        self.credentials = credentials

        var loaded: [HubProfile] = []
        if let data = defaults.data(forKey: profilesKey),
           let decoded = try? JSONDecoder().decode([HubProfile].self, from: data) {
            loaded = decoded
        }

        if let idx = loaded.firstIndex(where: { $0.id == HubProfile.localDefaultID || $0.isLocalDefault }) {
            // Keep discovered port up to date on the local default entry if no custom endpoints were added
            if loaded[idx].lanURLs.isEmpty && loaded[idx].publicURL == nil {
                loaded[idx] = defaultProfile
            }
        } else {
            loaded.insert(defaultProfile, at: 0)
        }

        let savedActive = defaults.string(forKey: activeKey)
        if let savedActive, loaded.contains(where: { $0.id == savedActive }) {
            self.activeHubID = savedActive
        } else {
            self.activeHubID = loaded.first?.id ?? defaultProfile.id
        }
        self.profiles = loaded
        persist()
    }

    public var activeProfile: HubProfile {
        profiles.first(where: { $0.id == activeHubID }) ?? profiles[0]
    }

    public func selectHub(id: String) {
        guard profiles.contains(where: { $0.id == id }) else { return }
        activeHubID = id
        persist()
    }

    @discardableResult
    public func upsertProfile(_ profile: HubProfile, credential: String? = nil, makeActive: Bool = true) -> HubProfile {
        var updated = profile
        if let existingIdx = matchingIndex(for: profile) {
            let existing = profiles[existingIdx]
            updated.id = existing.id
            updated.isLocalDefault = existing.isLocalDefault
            if updated.name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                updated.name = existing.name
            }
            if updated.publicURL == nil {
                updated.publicURL = existing.publicURL
            }
            var mergedLan = updated.lanURLs
            for oldLan in existing.lanURLs where !mergedLan.contains(where: { URLNormalizer.canonicalOrigin($0) == URLNormalizer.canonicalOrigin(oldLan) }) {
                mergedLan.append(oldLan)
            }
            updated.lanURLs = mergedLan
            if updated.clientID == nil { updated.clientID = existing.clientID }
            if updated.userName == nil { updated.userName = existing.userName }
            if updated.lastKnownVersion == nil { updated.lastKnownVersion = existing.lastKnownVersion }
            profiles[existingIdx] = updated
        } else {
            profiles.append(updated)
        }

        if let credential {
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

    public func updateAuthMetadata(id: String, clientID: String?, userName: String?, credential: String?) {
        guard let idx = profiles.firstIndex(where: { $0.id == id }) else { return }
        profiles[idx].clientID = clientID
        profiles[idx].userName = userName
        credentials.setCredential(credential, for: id)
        persist()
    }

    public func removeHub(id: String) {
        guard let idx = profiles.firstIndex(where: { $0.id == id }),
              !profiles[idx].isLocalDefault else {
            return
        }
        credentials.setCredential(nil, for: id)
        profiles.remove(at: idx)
        if activeHubID == id {
            activeHubID = profiles.first?.id ?? HubProfile.localDefaultID
        }
        persist()
    }

    public func credential(for id: String) -> String? {
        credentials.credential(for: id)
    }

    private func matchingIndex(for candidate: HubProfile) -> Int? {
        if let byID = profiles.firstIndex(where: { $0.id == candidate.id }) {
            return byID
        }
        let candidateOrigins = Set(candidate.orderedEndpoints.map(URLNormalizer.canonicalOrigin))
        guard !candidateOrigins.isEmpty else { return nil }
        return profiles.firstIndex { existing in
            let existingOrigins = Set(existing.orderedEndpoints.map(URLNormalizer.canonicalOrigin))
            return !existingOrigins.isDisjoint(with: candidateOrigins)
        }
    }

    private func persist() {
        if let encoded = try? JSONEncoder().encode(profiles) {
            defaults.set(encoded, forKey: profilesKey)
        }
        defaults.set(activeHubID, forKey: activeKey)
    }
}
