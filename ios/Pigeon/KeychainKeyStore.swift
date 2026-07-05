// KeychainKeyStore (M5.3) — the iOS analogue of Android's `AndroidKeyStore`.
//
// This is the core's `KeyStore` (put/get/delete), backed by the iOS Keychain.
// It is where the session token and the encrypted MLS device-state blob live on
// device (CLAUDE.md Gotcha #1): the Rust core references this store through the
// callback and hands us opaque bytes — the app never inspects or holds the
// secrets itself. Backend failures surface to the core as `KeyStoreError.Backend`
// (→ `CoreError.Storage`), exactly as the Android side maps them.
//
// Items are stored as `kSecClassGenericPassword` under a fixed service, keyed by
// the core's key string. Accessibility is `AfterFirstUnlockThisDeviceOnly`:
// readable by the background sync loop after the first unlock, but never included
// in an iCloud/iTunes backup and never leaving this device (Gotcha #1).
import Foundation
import Security
import PigeonCore

final class KeychainKeyStore: KeyStore {
    /// Namespaces all of this app's keychain items; mirrors Android's
    /// `pigeon_secure_prefs` prefs file.
    private let service: String

    init(service: String = "com.pigeon.mobile.keystore") {
        self.service = service
    }

    /// The base query identifying one item by (service, account=key).
    private func query(_ key: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: key,
        ]
    }

    func put(key: String, value: Data) throws {
        // Replace semantics (like EncryptedSharedPreferences.putString): try to
        // update an existing item first, else add. Avoids a delete+add race and
        // the duplicate-item error.
        let attrs: [String: Any] = [
            kSecValueData as String: value,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ]
        let updateStatus = SecItemUpdate(query(key) as CFDictionary, attrs as CFDictionary)
        switch updateStatus {
        case errSecSuccess:
            return
        case errSecItemNotFound:
            var addQuery = query(key)
            addQuery[kSecValueData as String] = value
            addQuery[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
            let addStatus = SecItemAdd(addQuery as CFDictionary, nil)
            guard addStatus == errSecSuccess else {
                throw KeyStoreError.Backend(reason: "put failed: \(message(addStatus))")
            }
        default:
            throw KeyStoreError.Backend(reason: "put failed: \(message(updateStatus))")
        }
    }

    func get(key: String) throws -> Data? {
        var q = query(key)
        q[kSecReturnData as String] = true
        q[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: AnyObject?
        let status = SecItemCopyMatching(q as CFDictionary, &result)
        switch status {
        case errSecSuccess:
            return result as? Data
        case errSecItemNotFound:
            return nil
        default:
            throw KeyStoreError.Backend(reason: "get failed: \(message(status))")
        }
    }

    func delete(key: String) throws {
        let status = SecItemDelete(query(key) as CFDictionary)
        // Deleting an absent key is a no-op, not an error (matches the core's
        // contract and Android's `remove`).
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw KeyStoreError.Backend(reason: "delete failed: \(message(status))")
        }
    }

    /// A human-readable reason for an `OSStatus` (never logged with secrets).
    private func message(_ status: OSStatus) -> String {
        (SecCopyErrorMessageString(status, nil) as String?) ?? "OSStatus \(status)"
    }
}
