import Foundation
import Network
import RunodeProtocol

/// 按证书指纹在局域网里找电脑现在的地址。
public protocol HostDiscovery: Sendable {
    /// `timeout` 内找到 TXT 里 `fp` 对得上的服务就返回它，找不到返回 `nil`。
    func locate(_ fingerprint: CertificateFingerprint, timeout: Duration) async -> TransportTarget?
    /// 一直等到这台电脑在局域网里公布服务（电脑离线时连接据此知道它回来了），只有取消才提前返回。
    func awaitPresence(_ fingerprint: CertificateFingerprint) async
}

extension HostDiscovery {
    /// 默认一直浏览到找到为止。
    public func awaitPresence(_ fingerprint: CertificateFingerprint) async {
        while !Task.isCancelled {
            if await locate(fingerprint, timeout: .seconds(3600)) != nil { return }
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

/// 用 Bonjour 浏览 `_runode._tcp`：电脑开着远程访问时公布这个服务，TXT 记录里有 `v=1` 和 `fp=<指纹>`。
/// 要 Info.plist 里的 `NSLocalNetworkUsageDescription` 和 `NSBonjourServices`。
public struct BonjourDiscovery: HostDiscovery {
    public static let serviceType = "_runode._tcp"

    public init() {}

    public func locate(_ fingerprint: CertificateFingerprint, timeout: Duration) async -> TransportTarget? {
        let browser = NWBrowser(for: .bonjourWithTXTRecord(type: Self.serviceType, domain: nil), using: .tcp)
        let queue = DispatchQueue(label: "dev.runode.bonjour")
        let once = OnceFlag()
        return await withTaskCancellationHandler {
            await withCheckedContinuation { (continuation: CheckedContinuation<TransportTarget?, Never>) in
                let finish: @Sendable (TransportTarget?) -> Void = { target in
                    if once.claim() {
                        browser.cancel()
                        continuation.resume(returning: target)
                    }
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
            browser.cancel()
        }
    }
}
