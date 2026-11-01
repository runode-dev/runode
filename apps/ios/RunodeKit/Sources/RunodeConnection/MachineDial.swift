import Foundation
import RunodeProtocol

/// 连上 `machine`、过门禁后在连接上做 `handshake`，返回它的结果。Bonjour 找到的地址、记着的地址（上次成功的、
/// 二维码里的，按顺序试）和跨网络的 iroh 同时试，谁先握完手用谁：组播不通的 Wi-Fi 上 Bonjour 要等满超时，
/// 记着的地址不跟着等；iroh 晚 `tunnelDelay` 才拨，在同一个局域网里时直连先到。晚到的那条也握完了手就交给
/// `close`。不能重试的失败（`LinkFailure.isFatal`）不等别的几条、直接报；都失败时报先到的那个不是「没有地址」
/// 的错，有限速的拒绝时报它，调用方才会等最长的退避。
func dial<Connection: Sendable>(
    _ machine: MachineRecord, keyStore: any DeviceKeyStore, discovery: any HostDiscovery,
    open: @escaping TransportOpener, tunnel: @escaping TunnelOpener,
    handshake: @escaping @Sendable (any FrameTransport, StoredDeviceKey) async throws -> Connection,
    close: @escaping @Sendable (Connection) -> Void
) async throws -> Connection {
    guard let key = try keyStore.key(for: machine.id) else { throw LinkFailure.missingKey }
    let attempt: @Sendable (@Sendable () async throws -> any FrameTransport) async throws -> Connection = { connect in
        let transport = try await connect()
        return try await withTaskCancellationHandler {
            do {
                return try await handshake(transport, key)
            } catch {
                transport.close()
                throw error
            }
        } onCancel: {
            transport.close()
        }
    }
    let fallback = machine.fallbackTargets
    let fingerprint = machine.fingerprint
    return try await withThrowingTaskGroup(of: Connection.self) { group in
        group.addTask {
            try await firstSuccess(of: fallback) { target in try await attempt { try await open(target, fingerprint) } }
        }
        group.addTask {
            guard let found = await discovery.locate(fingerprint, timeout: .seconds(2)) else {
                throw LinkFailure.noAddress
            }
            return try await attempt { try await open(found, fingerprint) }
        }
        if let node = machine.node {
            group.addTask {
                try await Task.sleep(for: tunnelDelay)
                return try await attempt { try await tunnel(node, fingerprint) }
            }
        }
        var winner: Connection?
        var failure: (any Error)?
        var fatal: LinkFailure?
        // 有了结果也把另一条等完：它可能也连上了，要关掉，不能留着一条没人读的连接。
        while let result = await group.nextResult() {
            switch result {
            case .success(let connection) where winner == nil && fatal == nil:
                winner = connection
                group.cancelAll()
            case .success(let connection):
                close(connection)
            case .failure(let error as LinkFailure) where error.isFatal && winner == nil:
                fatal = fatal ?? error
                group.cancelAll()
            case .failure(let error):
                if failure == nil || (failure as? LinkFailure) == .noAddress
                    || (error as? LinkFailure) == .rejected(.rateLimited)
                {
                    failure = error
                }
            }
        }
        if let fatal { throw fatal }
        if let winner { return winner }
        throw failure ?? LinkFailure.noAddress
    }
}
