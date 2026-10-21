import Foundation
import LocalAuthentication
import Security
import Synchronization

/// 远程桌面里替用户解开锁屏的电脑登录密码，每台电脑一个，只存在手机上。
public protocol UnlockPasswordStore: Sendable {
    /// 有没有存着，不验证身份。
    func hasPassword(for machine: UUID) -> Bool
    /// 验过身份（Face ID 或手机密码）后取出；没存着时为空，用户取消验证时抛错。
    func password(for machine: UUID, reason: String) async throws -> String?
    func save(_ password: String, for machine: UUID) throws
    func deletePassword(for machine: UUID)
}

/// 只记在内存里，测试用；取的时候不验证身份。
public final class MemoryUnlockPasswords: UnlockPasswordStore {
    private let passwords = Mutex<[UUID: String]>([:])

    public init() {}

    public func hasPassword(for machine: UUID) -> Bool { passwords.withLock { $0[machine] != nil } }
    public func password(for machine: UUID, reason: String) async throws -> String? {
        passwords.withLock { $0[machine] }
    }
    public func save(_ password: String, for machine: UUID) throws { passwords.withLock { $0[machine] = password } }
    public func deletePassword(for machine: UUID) { _ = passwords.withLock { $0.removeValue(forKey: machine) } }
}

/// 存进 Keychain 的通用密码项，每次读都要验证身份（`userPresence`：Face ID，不行时手机密码）；只在设了手机
/// 密码的本机上可读，不随备份迁走，关掉手机密码时跟着删掉。
public struct KeychainUnlockPasswords: UnlockPasswordStore {
    public let service: String

    public init(service: String = "dev.runode.mobile.unlock-password") {
        self.service = service
    }

    private func query(for machine: UUID) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: machine.uuidString,
        ]
    }

    public func hasPassword(for machine: UUID) -> Bool {
        let context = LAContext()
        context.interactionNotAllowed = true
        var lookup = query(for: machine)
        lookup[kSecUseAuthenticationContext as String] = context
        let status = SecItemCopyMatching(lookup as CFDictionary, nil)
        // 要验证身份才读得出来的项不让交互时报 `errSecInteractionNotAllowed`，说明它在。
        return status == errSecSuccess || status == errSecInteractionNotAllowed
    }

    public func password(for machine: UUID, reason: String) async throws -> String? {
        let lookup = query(for: machine)
        // 读的时候系统弹 Face ID，会卡住线程，放到别的线程上。
        return try await Task.detached {
            let context = LAContext()
            context.localizedReason = reason
            var request = lookup
            request[kSecReturnData as String] = true
            request[kSecMatchLimit as String] = kSecMatchLimitOne
            request[kSecUseAuthenticationContext as String] = context
            var result: CFTypeRef?
            let status = SecItemCopyMatching(request as CFDictionary, &result)
            if status == errSecItemNotFound { return nil }
            guard status == errSecSuccess, let data = result as? Data else {
                throw LinkFailure.keychain(Self.describe(status))
            }
            return String(decoding: data, as: UTF8.self)
        }.value
    }

    public func save(_ password: String, for machine: UUID) throws {
        deletePassword(for: machine)
        var error: Unmanaged<CFError>?
        guard
            let access = SecAccessControlCreateWithFlags(
                nil, kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly, .userPresence, &error)
        else { throw LinkFailure.keychain(String(describing: error?.takeRetainedValue())) }
        var add = query(for: machine)
        add[kSecValueData as String] = Data(password.utf8)
        add[kSecAttrAccessControl as String] = access
        let status = SecItemAdd(add as CFDictionary, nil)
        guard status == errSecSuccess else { throw LinkFailure.keychain(Self.describe(status)) }
    }

    public func deletePassword(for machine: UUID) {
        SecItemDelete(query(for: machine) as CFDictionary)
    }

    private static func describe(_ status: OSStatus) -> String {
        (SecCopyErrorMessageString(status, nil) as String?) ?? "OSStatus \(status)"
    }
}
