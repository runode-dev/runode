import Foundation
import Network
import Synchronization
import RunodeProtocol

/// 按证书指纹在局域网里找电脑现在的地址。
public protocol HostDiscovery: Sendable {
    /// `timeout` 内找到 TXT 里 `fp` 对得上的服务就返回它，找不到返回 `nil`。
    func locate(_ fingerprint: CertificateFingerprint, timeout: Duration) async -> TransportTarget?
    /// 一直等到这台电脑在局域网里公布服务（电脑离线时连接据此知道它回来了），只有取消才提前返回。
    func awaitPresence(_ fingerprint: CertificateFingerprint) async
}

extension HostDiscovery {
    /// 默认一直浏览到找到为止。浏览器出错（比如没有本地网络权限）时 `locate` 会马上返回空，歇一会儿再开，
    /// 不空转。
    public func awaitPresence(_ fingerprint: CertificateFingerprint) async {
        while !Task.isCancelled {
            if await locate(fingerprint, timeout: .seconds(3600)) != nil { return }
            try? await Task.sleep(for: .seconds(30))
        }
    }

    /// 连这台电脑时要挨个试的地址：Bonjour 找到的在前，再是 `fallback`。
    func targets(for fingerprint: CertificateFingerprint, then fallback: [TransportTarget]) async -> [TransportTarget] {
        let found = await locate(fingerprint, timeout: .seconds(2))
        return [found].compactMap { $0 } + fallback
    }
}

/// 按顺序试 `targets`，返回第一个成功的结果。不能重试的失败（`LinkFailure.isFatal`）和取消直接抛，
/// 其余的记下，都不成时抛最后一个；一个地址也没有时抛 `LinkFailure.noAddress`。
func firstSuccess<Result>(
    of targets: [TransportTarget], isolation: isolated (any Actor)? = #isolation,
    attempt: (TransportTarget) async throws -> Result
) async throws -> Result {
    var lastError: any Error = LinkFailure.noAddress
    for target in targets {
        try Task.checkCancellation()
        do {
            return try await attempt(target)
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

/// 不找，直接用记下的地址；测试和没有局域网权限时用。
public struct NoDiscovery: HostDiscovery {
    public init() {}

    public func locate(_ fingerprint: CertificateFingerprint, timeout: Duration) async -> TransportTarget? {
        nil
    }

    /// 找不着，只能等别的事（兜底的定时、用户点重试）来叫。
    public func awaitPresence(_ fingerprint: CertificateFingerprint) async {
        while !Task.isCancelled {
            try? await Task.sleep(for: .seconds(3600))
        }
    }
}

/// `BonjourDiscovery.locate` 里等结果的一方和给结果的一方谁先到都行。
private struct Waiting {
    var continuation: CheckedContinuation<TransportTarget?, Never>?
    /// 已经有结果了（外层是「有没有结果」，里层是找到的地址或空）。
    var result: TransportTarget??
}

/// 用 Bonjour 浏览 `_runode._tcp`：电脑开着远程访问时公布这个服务，TXT 记录里有 `v=1` 和 `fp=<指纹>`。
/// 要 Info.plist 里的 `NSLocalNetworkUsageDescription` 和 `NSBonjourServices`。
public struct BonjourDiscovery: HostDiscovery {
    public static let serviceType = "_runode._tcp"

    public init() {}

    public func locate(_ fingerprint: CertificateFingerprint, timeout: Duration) async -> TransportTarget? {
        let browser = NWBrowser(for: .bonjourWithTXTRecord(type: Self.serviceType, domain: nil), using: .tcp)
        let queue = DispatchQueue(label: "dev.runode.bonjour")
        let once = OnceFlag()
        // 取消时直接让等待返回：取消可能抢在 `browser.start` 之前到，那时没有人保证 `.cancelled` 的回调还会来，
        // 不能只靠回调里的 `finish`，不然要等满 `timeout`（离线等待时是一小时）。
        let pending = Mutex<Waiting>(Waiting())
        let finish: @Sendable (TransportTarget?) -> Void = { target in
            guard once.claim() else { return }
            browser.cancel()
            // 等待方还没登记（取消抢在前头）时记下结果，它登记时自己取。
            let waiting = pending.withLock { waiting in
                waiting.result = .some(target)
                return waiting.continuation
            }
            waiting?.resume(returning: target)
        }
        return await withTaskCancellationHandler {
            await withCheckedContinuation { (continuation: CheckedContinuation<TransportTarget?, Never>) in
                let done = pending.withLock { waiting -> TransportTarget?? in
                    if let result = waiting.result { return result }
                    waiting.continuation = continuation
                    return nil
                }
                if let done {
                    continuation.resume(returning: done)
                    return
                }
                if Task.isCancelled {
                    finish(nil)
                    return
                }
                browser.browseResultsChangedHandler = { results, _ in
                    for result in results {
                        guard case .bonjour(let txt) = result.metadata, let fp = txt["fp"],
                            CertificateFingerprint(text: fp) == fingerprint
                        else { continue }
                        var label = "Bonjour"
                        if case .service(let name, _, _, _) = result.endpoint {
                            label = "Bonjour：\(name)"
                        }
                        finish(TransportTarget(endpoint: result.endpoint, label: label))
                        return
                    }
                }
                browser.stateUpdateHandler = { state in
                    switch state {
                    case .failed, .cancelled: finish(nil)
                    default: break
                    }
                }
                browser.start(queue: queue)
                let seconds = Double(timeout.components.seconds) + Double(timeout.components.attoseconds) / 1e18
                queue.asyncAfter(deadline: .now() + seconds) { finish(nil) }
            }
        } onCancel: {
            finish(nil)
        }
    }
}
