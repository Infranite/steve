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
    /// so an untrusted `steve://pair` URI cannot spoof multi-line alert text.
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

    /// Parses `steve://pair?v=1&code=stp_...&name=...&pub=...&lan=...` into a typed `PairingPayload`.
    public static func parse(_ raw: String) -> PairingPayload? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let components = URLComponents(string: trimmed),
              components.scheme?.lowercased() == "steve",
              components.host?.lowercased() == "pair" else {
            return nil
        }
        let items = components.queryItems ?? []
        guard let codeItem = items.first(where: { $0.name == "code" })?.value?.trimmingCharacters(in: .whitespacesAndNewlines),
              codeItem.hasPrefix(codePrefix),
              codeItem.count >= 12 else {
            return nil
        }
        let version = items.first(where: { $0.name == "v" })?.value.flatMap(Int.init) ?? 1
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
            guard let val = item.value else { continue }
            for part in val.split(separator: ",") {
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
