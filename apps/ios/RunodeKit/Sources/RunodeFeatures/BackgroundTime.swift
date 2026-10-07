import Foundation

#if os(iOS)
    import UIKit
#endif

/// App 进后台以后向系统多要一会儿运行时间：iOS 上是 `UIApplication.beginBackgroundTask`
/// （`SystemBackgroundTime`），测试和演示模式里要不到（`NoBackgroundTime`）。
@MainActor
public protocol BackgroundTime: AnyObject {
    /// 申请多跑一会儿。快到期时系统调 `expiration`，等它做完才把这段时间交回去；申请不到时返回假，
    /// `expiration` 不会被调。
    func begin(expiration: @escaping @MainActor @Sendable () async -> Void) -> Bool
    /// 回到前台了：交回还没用完的时间，之后到期也不再调先前的 `expiration`。
    func end()
}

/// 要不到后台时间：一进后台就照原来那样断开。
public final class NoBackgroundTime: BackgroundTime {
    public init() {}

    public func begin(expiration: @escaping @MainActor @Sendable () async -> Void) -> Bool {
        false
    }

    public func end() {}
}

#if os(iOS)
    public final class SystemBackgroundTime: BackgroundTime {
        private var task: UIBackgroundTaskIdentifier = .invalid
        /// 每申请一次加一：到期的那次做完时，App 已经回过前台、又申请了新的，就不去交回新的那段。
        private var generation = 0

        public init() {}

        public func begin(expiration: @escaping @MainActor @Sendable () async -> Void) -> Bool {
            end()
            let current = generation
            task = UIApplication.shared.beginBackgroundTask(withName: "runode.connections") { [weak self] in
                Task { @MainActor in
                    await expiration()
                    if self?.generation == current { self?.end() }
                }
            }
            return task != .invalid
        }

        public func end() {
            generation += 1
            guard task != .invalid else { return }
            UIApplication.shared.endBackgroundTask(task)
            task = .invalid
        }
    }
#endif
