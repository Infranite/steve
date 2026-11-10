import Foundation
import Security

public protocol CredentialStoring: Sendable {
    func credential(for hubID: String) -> String?
    func setCredential(_ credential: String?, for hubID: String)
}

public final class InMemoryCredentialStore: CredentialStoring, @unchecked Sendable {
    private let lock = NSLock()
    private var storage: [String: String] = [:]

    public init(initial: [String: String] = [:]) {
        self.storage = initial
    }

    public func credential(for hubID: String) -> String? {
        lock.lock()
        defer { lock.unlock() }
        return storage[hubID]
    }

    public func setCredential(_ credential: String?, for hubID: String) {
        lock.lock()
        defer { lock.unlock() }
        if let credential, !credential.isEmpty {
            storage[hubID] = credential
        } else {
            storage.removeValue(forKey: hubID)
        }
    }
}

public final class KeychainCredentialStore: CredentialStoring, Sendable {
    private let service: String

    public init(service: String = "app.steve.client.credentials") {
        self.service = service
    }

    public func credential(for hubID: String) -> String? {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: hubID,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        guard status == errSecSuccess,
              let data = result as? Data,
              let string = String(data: data, encoding: .utf8),
              !string.isEmpty else {
            return nil
        }
        return string
    }

    public func setCredential(_ credential: String?, for hubID: String) {
        let deleteQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: hubID,
        ]
        SecItemDelete(deleteQuery as CFDictionary)

        guard let credential, !credential.isEmpty,
              let data = credential.data(using: .utf8) else {
            return
        }
        let addQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: hubID,
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlock,
        ]
        _ = SecItemAdd(addQuery as CFDictionary, nil)
    }
}
