import Foundation

public struct PairingPayload: Sendable, Equatable {
    public static let codePrefix = "stp_"
    public static let maxNameLength = 64

    public var version: Int
    public var code: String
    public var name: String?
    public var publicURL: URL?
    public var lanURLs: [URL]

    public init(version: Int = 1, code: String, name: String? = nil, publicURL: URL? = nil, lanURLs: [URL] = []) {
        self.version = version
        self.code = code
        self.name = Self.sanitizeName(name)
        self.publicURL = publicURL
        self.lanURLs = lanURLs
    }

    /// Strips control/format characters and newlines, collapses internal whitespace, and caps length
    /// so an untrusted pairing link cannot spoof multi-line alert text.
    public static func sanitizeName(_ raw: String?) -> String? {
        guard let raw else { return nil }
        let forbidden = CharacterSet.controlCharacters
            .union(.newlines)
            .union(.illegalCharacters)
        var cleanedScalars = String.UnicodeScalarView()
        for scalar in raw.unicodeScalars {
            if forbidden.contains(scalar) || scalar.properties.isDefaultIgnorableCodePoint {
                if CharacterSet.newlines.contains(scalar) || scalar == "\t" {
                    cleanedScalars.append(" ")
                }
                continue
            }
            cleanedScalars.append(scalar)
        }
        let collapsed = String(cleanedScalars)
            .components(separatedBy: .whitespacesAndNewlines)
            .filter { !$0.isEmpty }
            .joined(separator: " ")
        guard !collapsed.isEmpty else { return nil }
        return String(collapsed.prefix(maxNameLength))
    }

    /// Parses either:
    /// - `steve://pair?v=1&code=stp_...&name=...&pub=...&lan=...`
    /// - `http(s)://<any host>[:port][/<path>]#/pair?v=1&code=stp_...&name=...&pub=...&lan=...`
    ///
    /// The outer HTTP(S) origin is never treated as a Hub endpoint; only `pub` and `lan` inside the
    /// pairing query are used, so links passed from `http://127.0.0.1:<gateway>/#/pair?...` remain safe.
    public static func parse(_ raw: String) -> PairingPayload? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let rawQuery = extractRawPairingQuery(from: trimmed) else {
            return nil
        }
        let items = decodeFormQueryItems(rawQuery)
        guard let codeItem = items.first(where: { $0.name == "code" })?.value.trimmingCharacters(in: .whitespacesAndNewlines),
              codeItem.hasPrefix(codePrefix),
              codeItem.count >= 12 else {
            return nil
        }
        let version = items.first(where: { $0.name == "v" }).flatMap { Int($0.value) } ?? 1
        let name = sanitizeName(items.first(where: { $0.name == "name" })?.value)

        let pubURL: URL?
        if let rawPub = items.first(where: { $0.name == "pub" })?.value {
            pubURL = URLNormalizer.normalize(rawPub)
        } else {
            pubURL = nil
        }

        let pubOrigin = pubURL.map(URLNormalizer.canonicalOrigin)
        var lanURLs: [URL] = []
        var seen = Set<String>()
        if let pubOrigin {
            seen.insert(pubOrigin)
        }
        for item in items where item.name == "lan" {
            for part in item.value.split(separator: ",") {
                if let url = URLNormalizer.normalize(String(part)) {
                    let key = URLNormalizer.canonicalOrigin(url)
                    if seen.insert(key).inserted {
                        lanURLs.append(url)
                    }
                }
            }
        }

        guard pubURL != nil || !lanURLs.isEmpty else {
            return nil
        }

        return PairingPayload(
            version: version,
            code: codeItem,
            name: name,
            publicURL: pubURL,
            lanURLs: lanURLs
        )
    }

    private static func extractRawPairingQuery(from trimmed: String) -> String? {
        guard let components = URLComponents(string: trimmed),
              let scheme = components.scheme?.lowercased() else {
            return nil
        }
        if scheme == "steve" {
            guard components.host?.lowercased() == "pair",
                  components.path.isEmpty || components.path == "/" else {
                return nil
            }
            return components.percentEncodedQuery
        }
        if scheme == "http" || scheme == "https" {
            guard let host = components.host, !host.isEmpty,
                  let fragment = components.percentEncodedFragment else {
                return nil
            }
            if fragment.hasPrefix("/pair?") {
                return String(fragment.dropFirst("/pair?".count))
            }
            if fragment.hasPrefix("/pair/?") {
                return String(fragment.dropFirst("/pair/?".count))
            }
            return nil
        }
        return nil
    }

    /// Decodes an `application/x-www-form-urlencoded` query string (`URLSearchParams` semantics:
    /// split `&` and `=` before percent-decoding, decode `+` as space).
    private static func decodeFormQueryItems(_ rawQuery: String) -> [(name: String, value: String)] {
        var result: [(name: String, value: String)] = []
        for pair in rawQuery.split(separator: "&", omittingEmptySubsequences: true) {
            let rawKey: Substring
            let rawValue: Substring
            if let eq = pair.firstIndex(of: "=") {
                rawKey = pair[..<eq]
                rawValue = pair[pair.index(after: eq)...]
            } else {
                rawKey = pair
                rawValue = ""
            }
            let key = String(rawKey).replacingOccurrences(of: "+", with: "%20").removingPercentEncoding
            let value = String(rawValue).replacingOccurrences(of: "+", with: "%20").removingPercentEncoding
            if let key, let value {
                result.append((name: key, value: value))
            }
        }
        return result
    }

    /// Ordered candidate endpoints:
    /// - Public network first by default when `publicURL != nil` (with LAN fallback).
    /// - LAN / internal network when `publicURL == nil`.
    public var orderedEndpoints: [URL] {
        var result: [URL] = []
        if let pub = publicURL {
            result.append(pub)
        }
        result.append(contentsOf: lanURLs)
        return result
    }

    public var usesPublicByDefault: Bool {
        publicURL != nil
    }
}
