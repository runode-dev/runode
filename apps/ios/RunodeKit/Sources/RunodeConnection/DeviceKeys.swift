import CryptoKit
import Foundation
import Security

/// 一台 Mac 对应的设备签名私钥（P-256）。只存私钥的数据：Secure Enclave 的是加密过的句柄（离开
/// 这部手机的 Secure Enclave 就没用），软件的是原始私钥。每次签名时现场还原，这个值本身只是数据，
/// 能在各个并发域之间传。
public struct StoredDeviceKey: Hashable, Sendable, Codable {
    public enum Kind: String, Sendable, Codable {
        /// 真机上的 Secure Enclave 密钥。
        case secureEnclave
        /// 模拟器这类没有 Secure Enclave 的设备上退回的软件密钥。
        case software
    }

    public var kind: Kind
    public var data: Data

    public init(kind: Kind, data: Data) {
        self.kind = kind
        self.data = data
    }

    /// 新生成一把：有 Secure Enclave 就用它，否则用软件密钥。
    public static func generate() throws -> StoredDeviceKey {
        if SecureEnclave.isAvailable {
            let key = try SecureEnclave.P256.Signing.PrivateKey()
            return StoredDeviceKey(kind: .secureEnclave, data: key.dataRepresentation)
        }
        return StoredDeviceKey(kind: .software, data: P256.Signing.PrivateKey().rawRepresentation)
    }

    /// 公钥，X9.63 未压缩格式（65 字节），配对时发给 Mac。
    public var publicKeyX963: Data {
        get throws {
            switch kind {
            case .secureEnclave:
                try SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: data).publicKey.x963Representation
            case .software:
                try P256.Signing.PrivateKey(rawRepresentation: data).publicKey.x963Representation
            }
        }
    }

    /// ECDSA P-256 + SHA-256 签名，DER 编码。
    public func signature(for payload: Data) throws -> Data {
        switch kind {
        case .secureEnclave:
            try SecureEnclave.P256.Signing.PrivateKey(dataRepresentation: data).signature(for: payload)
                .derRepresentation
        case .software:
            try P256.Signing.PrivateKey(rawRepresentation: data).signature(for: payload).derRepresentation
        }
    }
}

/// 设备私钥放在哪里。真的实现是 Keychain；测试用内存里的。
public protocol DeviceKeyStore: Sendable {
    func save(_ key: StoredDeviceKey, for machine: UUID) throws
    func key(for machine: UUID) throws -> StoredDeviceKey?
    func deleteKey(for machine: UUID) throws
}

/// 把设备私钥存进 Keychain 的通用密码项：`service` 固定，`account` 是这台 Mac 记录的编号，
/// 只在本机首次解锁后可读，不随备份迁到别的设备。
public struct KeychainDeviceKeyStore: DeviceKeyStore {
    public let service: String

    public init(service: String = "dev.runode.mobile.device-key") {
        self.service = service
    }

    private func query(for machine: UUID) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: machine.uuidString,
        ]
    }

    public func save(_ key: StoredDeviceKey, for machine: UUID) throws {
        let data = try JSONEncoder().encode(key)
        var add = query(for: machine)
        add[kSecValueData as String] = data
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        var status = SecItemAdd(add as CFDictionary, nil)
        if status == errSecDuplicateItem {
            let update: [String: Any] = [
                kSecValueData as String: data,
                kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
            ]
            status = SecItemUpdate(query(for: machine) as CFDictionary, update as CFDictionary)
        }
        guard status == errSecSuccess else { throw LinkFailure.keychain(Self.describe(status)) }
    }

    public func key(for machine: UUID) throws -> StoredDeviceKey? {
        var lookup = query(for: machine)
        lookup[kSecReturnData as String] = true
        lookup[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: CFTypeRef?
        let status = SecItemCopyMatching(lookup as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else {
            throw LinkFailure.keychain(Self.describe(status))
        }
        return try JSONDecoder().decode(StoredDeviceKey.self, from: data)
    }

    public func deleteKey(for machine: UUID) throws {
        let status = SecItemDelete(query(for: machine) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw LinkFailure.keychain(Self.describe(status))
        }
    }

    private static func describe(_ status: OSStatus) -> String {
        (SecCopyErrorMessageString(status, nil) as String?) ?? "OSStatus \(status)"
    }
}
