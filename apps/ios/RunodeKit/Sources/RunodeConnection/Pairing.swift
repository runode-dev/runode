import Foundation
import RunodeProtocol

/// 拿着二维码里的邀请和电脑配对。
public protocol Pairing: Sendable {
    /// 配对成功时设备私钥已经存好，返回这台电脑的记录（还没存进 `MachineStore`）。
    func pair(with invitation: PairingInvitation, deviceName: String) async throws -> MachineRecord
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
        var targets: [TransportTarget] = []
        if let found = await discovery.locate(invitation.fingerprint, timeout: .seconds(2)) {
            targets.append(found)
        }
        targets += invitation.addresses.compactMap { TransportTarget.address($0, port: invitation.port) }
        guard !targets.isEmpty else { throw LinkFailure.noAddress }
        var lastError: any Error = LinkFailure.noAddress
        for target in targets {
            try Task.checkCancellation()
            do {
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
            } catch let failure as LinkFailure where failure.isFatal {
                throw failure
            } catch is CancellationError {
                throw CancellationError()
            } catch {
                lastError = error
            }
        }
        throw lastError
    }
}
