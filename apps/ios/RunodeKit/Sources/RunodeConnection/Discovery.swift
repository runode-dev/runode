import Foundation
import Network
import RunodeProtocol

/// 按证书指纹在局域网里找 Mac 现在的地址。
public protocol HostDiscovery: Sendable {
    /// `timeout` 内找到 TXT 里 `fp` 对得上的服务就返回它，找不到返回 `nil`。
    func locate(_ fingerprint: CertificateFingerprint, timeout: Duration) async -> TransportTarget?
}

/// 不找，直接用记下的地址；测试和没有局域网权限时用。
public struct NoDiscovery: HostDiscovery {
    public init() {}

    public func locate(_ fingerprint: CertificateFingerprint, timeout: Duration) async -> TransportTarget? {
        nil
    }
}

/// 用 Bonjour 浏览 `_runode._tcp`：Mac 开着远程访问时公布这个服务，TXT 记录里有 `v=1` 和 `fp=<指纹>`。
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
