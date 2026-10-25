import Foundation

/// 官方 App 为一台电脑向中转服务换推送句柄的请求体（宿主的 `push::RelayRegister`）。电脑只拿得到句柄，
/// 拿不到 device token。
public struct RelayRegister: Hashable, Sendable, Encodable {
    /// 宿主的 `push::RELAY_REGISTER`。
    public static let path = "/v1/register"
    /// 宿主的 `push::OFFICIAL_BUNDLE`：只有这个 App 的推送经中转服务发。
    public static let officialBundle = "dev.runode.mobile"

    public var v: UInt32 = 1
    public var env: ApnsEnv
    /// device token，十六进制。
    public var token: String
    /// 手机给这台电脑编的 UUID（`ClientMsg.pushRegister` 的 `machine`）。
    public var machine: String

    public init(env: ApnsEnv, token: String, machine: String) {
        self.env = env
        self.token = token
        self.machine = machine
    }
}

/// 中转服务换句柄的回话（宿主的 `push::RelayHandle`）。
public struct RelayHandle: Hashable, Sendable, Decodable {
    public var handle: String
}

/// 中转服务出错时的响应体（宿主的 `push::PushError`）。
public struct PushError: Hashable, Sendable, Decodable {
    public var reason: String
}
