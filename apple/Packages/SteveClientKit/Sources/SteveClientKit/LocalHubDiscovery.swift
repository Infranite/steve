import Foundation

public enum LocalHubDiscovery {
    public static let defaultPort = 8440

    /// Discovers the local Hub endpoint on launch without reading or exposing any secret material.
    /// Respects `STEVE_HUB_URL`, `STEVE_PORT`, or `STEVE_HOME/steve.env` when present, defaulting to `http://127.0.0.1:8440`.
    public static func defaultProfile(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        steveHome: URL? = nil
    ) -> HubProfile {
        let port = discoveredLocalPort(environment: environment, steveHome: steveHome) ?? defaultPort
        let displayName = L10n.tr("本机 Hub", "Local Hub")

        if let rawURL = environment["STEVE_HUB_URL"],
           let normalized = URLNormalizer.normalize(rawURL) {
            let route = HubRouteKind.classify(url: normalized)
            if route == .publicNetwork {
                return HubProfile(
                    id: HubProfile.localDefaultID,
                    name: displayName,
                    publicURL: normalized,
                    lanURLs: [URL(string: "http://127.0.0.1:\(port)")!],
                    isLocalDefault: true
                )
            } else {
                return HubProfile(
                    id: HubProfile.localDefaultID,
                    name: displayName,
                    publicURL: nil,
                    lanURLs: [normalized],
                    isLocalDefault: true
                )
            }
        }

        return HubProfile.makeLocalDefault(port: port)
    }

    public static func discoveredLocalPort(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        steveHome: URL? = nil
    ) -> Int? {
        if let rawPort = environment["STEVE_PORT"],
           let port = Int(rawPort.trimmingCharacters(in: .whitespacesAndNewlines)),
           (1...65535).contains(port) {
            return port
        }
        #if os(macOS)
        let homeDir: URL
        if let steveHome {
            homeDir = steveHome
        } else if let envHome = environment["STEVE_HOME"], !envHome.isEmpty {
            homeDir = URL(fileURLWithPath: envHome, isDirectory: true)
        } else {
            homeDir = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".steve", isDirectory: true)
        }
        for fileName in ["steve.env", "hub.env", "config.env"] {
            let envFile = homeDir.appendingPathComponent(fileName)
            guard let text = try? String(contentsOf: envFile, encoding: .utf8) else { continue }
            if let port = parsePort(fromEnvContents: text) {
                return port
            }
        }
        #endif
        return nil
    }

    public static func parsePort(fromEnvContents text: String) -> Int? {
        var found: Int?
        for rawLine in text.split(whereSeparator: \.isNewline) {
            var line = rawLine.trimmingCharacters(in: .whitespaces)
            guard !line.isEmpty, !line.hasPrefix("#") else { continue }
            if line.hasPrefix("export ") {
                line = String(line.dropFirst("export ".count)).trimmingCharacters(in: .whitespaces)
            }
            guard let eq = line.firstIndex(of: "=") else { continue }
            let key = line[..<eq].trimmingCharacters(in: .whitespaces)
            guard key == "STEVE_PORT" else { continue }
            var value = String(line[line.index(after: eq)...]).trimmingCharacters(in: .whitespaces)
            if let quote = value.first, quote == "\"" || quote == "'" {
                let rest = value.dropFirst()
                if let closeIdx = rest.firstIndex(of: quote) {
                    value = String(rest[..<closeIdx])
                }
            } else if let commentIdx = value.firstIndex(of: "#") {
                value = String(value[..<commentIdx]).trimmingCharacters(in: .whitespaces)
            }
            if let port = Int(value.trimmingCharacters(in: .whitespaces)), (1...65535).contains(port) {
                found = port
            }
        }
        return found
    }
}
