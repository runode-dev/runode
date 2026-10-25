import Foundation
import RunodeProtocol

/// 官方 App 的推送中转服务：给一台电脑换推送句柄，测试换成假的。
public protocol PushRelay: Sendable {
    func handle(for request: RelayRegister) async throws -> String
}

/// 中转服务回了 200 以外的状态，或者没回话（`status` 为 0）。
public struct PushRelayError: Error, Hashable, LocalizedError {
    public var status: Int
    public var reason: String

    public init(status: Int, reason: String) {
        self.status = status
        self.reason = reason
    }

    public var errorDescription: String? {
        "push relay: HTTP \(status)" + (reason.isEmpty ? "" : " \(reason)")
    }
}

/// 经 HTTPS 找中转服务换句柄。
public struct HTTPPushRelay: PushRelay {
    /// 官方中转服务。
    public static let official = HTTPPushRelay(base: URL(string: "https://push.runode.dev")!)
    /// Info.plist 里记中转服务基址的键，值来自 build setting `RUNODE_PUSH_RELAY`。
    public static let infoKey = "RunodePushRelay"
    /// 换一次句柄最多等这么久。
    static let timeout: TimeInterval = 10

    public var base: URL

    public init(base: URL) {
        self.base = base
    }

    /// 从 App 的 Info.plist 读基址；没写、build setting 没展开、不是 https 时用 `official`。
    public init(info: [String: Any]) {
        let raw = (info[Self.infoKey] as? String)?.trimmingCharacters(in: .whitespaces) ?? ""
        let url = URL(string: raw).flatMap { $0.scheme == "https" && $0.host() != nil ? $0 : nil }
        self.init(base: url ?? Self.official.base)
    }

    public func handle(for request: RelayRegister) async throws -> String {
        var post = URLRequest(url: base.appending(path: RelayRegister.path))
        post.httpMethod = "POST"
        post.timeoutInterval = Self.timeout
        post.setValue("application/json", forHTTPHeaderField: "Content-Type")
        post.httpBody = try JSONEncoder().encode(request)
        let (data, response) = try await URLSession.shared.data(for: post)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard status == 200 else {
            let reason = (try? JSONDecoder().decode(PushError.self, from: data))?.reason ?? ""
            throw PushRelayError(status: status, reason: reason)
        }
        return try JSONDecoder().decode(RelayHandle.self, from: data).handle
    }
}
