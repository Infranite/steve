import Combine
import Foundation

/// The two languages supported by the Web UI. Bridge values must match exactly.
public enum NativeShellLanguage: String, Sendable {
    case chinese = "zh-CN"
    case english = "en"
}

/// Synchronous readers (models, network errors and notifications) never need a MainActor hop.
final class NativeUILanguageSnapshot: @unchecked Sendable {
    private let lock = NSLock()
    private var chinese: Bool
    private let systemLanguages: () -> [String]

    init(systemLanguages: @escaping () -> [String] = { Locale.preferredLanguages }) {
        self.systemLanguages = systemLanguages
        self.chinese = Self.prefersChinese(systemLanguages())
    }

    var prefersChinese: Bool {
        lock.lock()
        defer { lock.unlock() }
        return chinese
    }

    func set(_ language: NativeShellLanguage?) -> Bool {
        let next = language.map { $0 == .chinese } ?? Self.prefersChinese(systemLanguages())
        lock.lock()
        defer { lock.unlock() }
        guard chinese != next else { return false }
        chinese = next
        return true
    }

    private static func prefersChinese(_ languages: [String]) -> Bool {
        languages.first.map { $0.lowercased().hasPrefix("zh") } ?? true
    }
}

/// One app-session mirror, not a saved preference or a per-Hub setting. Independent native
/// roots and Commands subscribe explicitly; a language change must never recreate the WebView.
@MainActor
public final class NativeUILanguageState: ObservableObject {
    public static let shared = NativeUILanguageState(snapshot: L10n.uiLanguageSnapshot)
    @Published public private(set) var prefersChinese: Bool
    private let snapshot: NativeUILanguageSnapshot

    init(snapshot: NativeUILanguageSnapshot) {
        self.snapshot = snapshot
        self.prefersChinese = snapshot.prefersChinese
    }

    func accept(_ language: NativeShellLanguage?) {
        // Update synchronous readers first, before publishing the view invalidation.
        guard snapshot.set(language) else { return }
        prefersChinese = snapshot.prefersChinese
    }
}
