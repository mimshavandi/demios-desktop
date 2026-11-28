import Foundation
import Security

/// This phone's keys in the iOS Keychain, for the Rust core (`KeyStore` in dn-mobile).
/// Items stay on this device: `WhenUnlockedThisDeviceOnly`, never synced to iCloud or
/// restored from a backup to another phone.
final class Keychain: KeyStore, @unchecked Sendable {
    private let service = "io.demios.phone"

    private func query(_ name: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: name,
            kSecAttrSynchronizable as String: kCFBooleanFalse as Any,
        ]
    }

    private func failure(_ status: OSStatus) -> MobileError {
        let text = SecCopyErrorMessageString(status, nil) as String? ?? "error \(status)"
        return .Keychain(reason: text)
    }

    func get(name: String) throws -> String? {
        var q = query(name)
        q[kSecReturnData as String] = true
        q[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        let status = SecItemCopyMatching(q as CFDictionary, &item)
        switch status {
        case errSecSuccess:
            guard let data = item as? Data, let text = String(data: data, encoding: .utf8) else {
                throw MobileError.Keychain(reason: "an item isn't readable text")
            }
            return text
        case errSecItemNotFound:
            return nil
        default:
            throw failure(status)
        }
    }

    func set(name: String, value: String) throws {
        let data = Data(value.utf8)
        let update = [kSecValueData as String: data]
        let status = SecItemUpdate(query(name) as CFDictionary, update as CFDictionary)
        if status == errSecItemNotFound {
            var add = query(name)
            add[kSecValueData as String] = data
            add[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
            let added = SecItemAdd(add as CFDictionary, nil)
            if added != errSecSuccess { throw failure(added) }
        } else if status != errSecSuccess {
            throw failure(status)
        }
    }

    func delete(name: String) throws -> Bool {
        let status = SecItemDelete(query(name) as CFDictionary)
        switch status {
        case errSecSuccess: return true
        case errSecItemNotFound: return false
        default: throw failure(status)
        }
    }
}
