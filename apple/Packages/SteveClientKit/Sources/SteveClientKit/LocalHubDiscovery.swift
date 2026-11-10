import Foundation

public enum LocalHubDiscovery {
    public static let defaultPort = 8440

    /// Discovers the local Hub endpoint on launch without reading or exposing any secret material.
    /// Respects `STEVE_HUB_URL`, `STEVE_PORT`, or `STEVE_HOME` when present, defaulting to `http://127.0.0.1:8440`.
    public static func defaultProfile(environment: [String: String] = ProcessInfo.processInfo.environment) -> HubProfile {
        if let rawURL = environment["STEVE_HUB_URL"],
           let normalized = URLNormalizer.normalize(rawURL) {
            let route = HubRouteKind.classify(url: normalized)
            if route == .publicNetwork {
                return HubProfile(
                    id: HubProfile.localDefaultID,
                    name: "本机 Hub",
                    publicURL: normalized,
                    lanURLs: [URL(string: "http://127.0.0.1:\(defaultPort)")!],
                    isLocalDefault: true
                )
            } else {
                return HubProfile(
                    id: HubProfile.localDefaultID,
                    name: "本机 Hub",
                    publicURL: nil,
                    lanURLs: [normalized],
                    isLocalDefault: true
                )
            }
        }

        let port = discoveredLocalPort(environment: environment) ?? defaultPort
        return HubProfile.makeLocalDefault(port: port)
    }

    private static func discoveredLocalPort(environment: [String: String]) -> Int? {
        if let rawPort = environment["STEVE_PORT"],
           let port = Int(rawPort.trimmingCharacters(in: .whitespacesAndNewlines)),
           (1...65535).contains(port) {
            return port
        }
        #if os(macOS)
        let homeDir: URL
        if let steveHome = environment["STEVE_HOME"], !steveHome.isEmpty {
            homeDir = URL(fileURLWithPath: steveHome, isDirectory: true)
        } else {
            homeDir = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".steve", isDirectory: true)
        }
        for fileName in ["hub.env", "config.env"] {
            let envFile = homeDir.appendingPathComponent(fileName)
            guard let text = try? String(contentsOf: envFile, encoding: .utf8) else { continue }
            for line in text.split(whereSeparator: \.isNewline) {
                let trimmed = line.trimmingCharacters(in: .whitespaces)
                guard !trimmed.hasPrefix("#") else { continue }
                if let range = trimmed.range(of: "STEVE_PORT=") {
                    let value = trimmed[range.upperBound...]
                        .trimmingCharacters(in: CharacterSet(charactersIn: "\"' \t"))
                    if let port = Int(value), (1...65535).contains(port) {
                        return port
                    }
                }
            }
        }
        #endif
        return nil
    }
}
