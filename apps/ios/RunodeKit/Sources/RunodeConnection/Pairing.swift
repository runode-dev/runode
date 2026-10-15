import Foundation
import RunodeProtocol

/// 拿着二维码里的邀请和电脑配对。
public protocol Pairing: Sendable {
    /// 配对成功时设备私钥已经存好，返回这台电脑的记录（还没存进 `MachineStore`）。
    func pair(with invitation: PairingInvitation, deviceName: String) async throws -> MachineRecord
    /// `pair` 配好了、调用方却不要了（配对页已经关掉）：删掉为它存的设备私钥，不然这把钥匙没有记录认领，
    /// 一直留在钥匙串里。
    func discard(_ machine: MachineRecord)
}

extension Pairing {
    /// 不存私钥的配对（测试、演示）没什么要删的。
    public func discard(_ machine: MachineRecord) {}
}

/// 真的配对：按 Bonjour 找到的服务和二维码里的地址挨个试，门禁里发 `remote_pair`。
/// 配对用的连接在放行后就关掉，之后按已配对的设备正常登录。
public struct RemotePairing: Pairing {
    private let keyStore: any DeviceKeyStore
    private let discovery: any HostDiscovery
    private let open: TransportOpener
    private let generateKey: @Sendable () throws -> StoredDeviceKey

    public init(
        keyStore: any DeviceKeyStore, discovery: any HostDiscovery,
        open: @escaping TransportOpener = { target, fingerprint in
            try await TLSChannel.open(to: target, fingerprint: fingerprint)
        },
        generateKey: @escaping @Sendable () throws -> StoredDeviceKey = { try StoredDeviceKey.generate() }
    ) {
        self.keyStore = keyStore
        self.discovery = discovery
        self.open = open
        self.generateKey = generateKey
    }

    public func pair(with invitation: PairingInvitation, deviceName: String) async throws -> MachineRecord {
        guard !invitation.isExpired() else { throw LinkFailure.invitationExpired }
        let key = try generateKey()
        let targets = await discovery.targets(
            for: invitation.fingerprint,
            then: invitation.addresses.compactMap { TransportTarget.address($0, port: invitation.port) })
        return try await firstSuccess(of: targets) { target in
            let transport = try await open(target, invitation.fingerprint)
            defer { transport.close() }
            let outcome = try await GateClient.run(
                over: transport, credential: .pair(secret: invitation.secret, deviceName: deviceName, key: key))
            let record = MachineRecord(
                name: invitation.hostName, hostName: outcome.hostName.isEmpty ? invitation.hostName : outcome.hostName,
                fingerprint: invitation.fingerprint, port: invitation.port, addresses: invitation.addresses,
                lastAddress: transport.remoteAddress, deviceId: outcome.deviceId)
            try keyStore.save(key, for: record.id)
            return record
        }
    }

    public func discard(_ machine: MachineRecord) {
        try? keyStore.deleteKey(for: machine.id)
    }
}
