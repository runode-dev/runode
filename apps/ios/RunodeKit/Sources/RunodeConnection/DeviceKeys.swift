import CryptoKit
import Foundation
import os
import Security
import Synchronization

/// 一台电脑对应的设备签名私钥（P-256）。只存私钥的数据：Secure Enclave 的是加密过的句柄（离开
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
    /// 跨网络连这台电脑时 iroh 端点的私钥（32 字节的 Ed25519 种子），每台电脑一把，手机的端点公钥对这台电脑
    /// 才一直不变。可能为空。
    public var tunnel: Data?

    public init(kind: Kind, data: Data, tunnel: Data? = nil) {
        self.kind = kind
        self.data = data
        self.tunnel = tunnel
    }

    /// 新生成一把：有 Secure Enclave 就用它，否则用软件密钥；iroh 端点的私钥一起生成。
    public static func generate() throws -> StoredDeviceKey {
        let tunnel = try newTunnelSecret()
        if SecureEnclave.isAvailable {
            let key = try SecureEnclave.P256.Signing.PrivateKey()
            return StoredDeviceKey(kind: .secureEnclave, data: key.dataRepresentation, tunnel: tunnel)
        }
        return StoredDeviceKey(kind: .software, data: P256.Signing.PrivateKey().rawRepresentation, tunnel: tunnel)
    }

    /// 新的 iroh 端点私钥：任意 32 字节都是合法的 Ed25519 种子。
    static func newTunnelSecret() throws -> Data {
        var bytes = [UInt8](repeating: 0, count: 32)
        let status = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        guard status == errSecSuccess else { throw LinkFailure.keychain("SecRandomCopyBytes \(status)") }
        return Data(bytes)
    }

    /// 公钥，X9.63 未压缩格式（65 字节），配对时发给电脑。
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

/// 存不进设备私钥的 iroh 端点私钥，按电脑记在这个进程里。读、补、存都在这把锁里排队，同时拨号的几条连接
/// 不会各生成一把。
private let temporaryTunnelSecrets = Mutex<[UUID: Data]>([:])
private let tunnelLog = Logger(subsystem: "dev.runode.mobile", category: "tunnel")

extension DeviceKeyStore {
    /// 跨网络连 `machine` 用的 iroh 端点私钥。存着的设备私钥里没有时新生成一把、存回去；存不进去时这把记在
    /// 进程里，下次照旧用它、再试着存。没有设备私钥时报 `LinkFailure.missingKey`，读不了时照样抛出，不生成新的
    /// 覆盖它。
    public func tunnelSecret(for machine: UUID) throws -> Data {
        try temporaryTunnelSecrets.withLock { temporary in
            guard var key = try key(for: machine) else { throw LinkFailure.missingKey }
            if let tunnel = key.tunnel {
                temporary[machine] = nil
                return tunnel
            }
            let secret = try temporary[machine] ?? StoredDeviceKey.newTunnelSecret()
            key.tunnel = secret
            do {
                try save(key, for: machine)
                temporary[machine] = nil
            } catch {
                temporary[machine] = secret
                tunnelLog.error("iroh 端点私钥存不进去，先用临时的：\(String(describing: error), privacy: .public)")
            }
            return secret
        }
    }

    /// 删掉 `machine` 的设备私钥，连同进程里为它记着的临时 iroh 端点私钥；返回跨网络连它用过的 iroh 端点私钥。
    /// 只在不再连这台电脑时调，所以删不掉时不报。
    public func deleteKeyAndTunnel(for machine: UUID) -> [Data] {
        temporaryTunnelSecrets.withLock { temporary in
            var secrets: [Data] = []
            if let stored = (try? key(for: machine))?.tunnel { secrets.append(stored) }
            if let secret = temporary.removeValue(forKey: machine), !secrets.contains(secret) { secrets.append(secret) }
            try? deleteKey(for: machine)
            return secrets
        }
    }
}

/// 只记在内存里，测试和演示模式用。
public final class MemoryDeviceKeyStore: DeviceKeyStore {
    private let keys = Mutex<[UUID: StoredDeviceKey]>([:])

    public init() {}

    public func save(_ key: StoredDeviceKey, for machine: UUID) throws { keys.withLock { $0[machine] = key } }
    public func key(for machine: UUID) throws -> StoredDeviceKey? { keys.withLock { $0[machine] } }
    public func deleteKey(for machine: UUID) throws { _ = keys.withLock { $0.removeValue(forKey: machine) } }
}

/// 把设备私钥存进 Keychain 的通用密码项：`service` 固定，`account` 是这台电脑记录的编号，
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
