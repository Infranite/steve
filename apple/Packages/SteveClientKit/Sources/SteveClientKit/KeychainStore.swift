import Foundation
import os
import Security

public enum CredentialStoreError: LocalizedError, Sendable, Equatable {
    case saveFailed

    public var errorDescription: String? {
        switch self {
        case .saveFailed:
            return L10n.tr(
                "无法将客户端凭证保存到系统钥匙串。",
                "Failed to save the client credential to Keychain."
            )
        }
    }
}

public protocol CredentialStoring: Sendable {
    func credential(for hubID: String) -> String?
    @discardableResult
    func setCredential(_ credential: String?, for hubID: String) -> Bool
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

    @discardableResult
    public func setCredential(_ credential: String?, for hubID: String) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        if let credential, !credential.isEmpty {
            storage[hubID] = credential
        } else {
            storage.removeValue(forKey: hubID)
        }
        return true
    }
}

public final class KeychainCredentialStore: CredentialStoring, Sendable {
    private static let logger = Logger(subsystem: "app.steve.client", category: "keychain")
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

    @discardableResult
    public func setCredential(_ credential: String?, for hubID: String) -> Bool {
        let deleteQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: hubID,
        ]
        let delStatus = SecItemDelete(deleteQuery as CFDictionary)
        if delStatus != errSecSuccess && delStatus != errSecItemNotFound {
            Self.logger.error("SecItemDelete failed for \(hubID, privacy: .public): status=\(delStatus)")
        }

        guard let credential, !credential.isEmpty else {
            return delStatus == errSecSuccess || delStatus == errSecItemNotFound
        }
        guard let data = credential.data(using: .utf8) else {
            return false
        }

        let addQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: hubID,
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ]
        let status = SecItemAdd(addQuery as CFDictionary, nil)
        if status == errSecDuplicateItem {
            let updateAttrs: [String: Any] = [
                kSecValueData as String: data,
                kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            ]
            let updateStatus = SecItemUpdate(deleteQuery as CFDictionary, updateAttrs as CFDictionary)
            if updateStatus != errSecSuccess {
                Self.logger.error("SecItemUpdate failed for \(hubID, privacy: .public): status=\(updateStatus)")
                return false
            }
            return true
        }
        if status != errSecSuccess {
            Self.logger.error("SecItemAdd failed for \(hubID, privacy: .public): status=\(status)")
            return false
        }
        return true
    }
}
