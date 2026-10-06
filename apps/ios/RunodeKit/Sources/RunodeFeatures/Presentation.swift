import Foundation
import RunodeConnection
import RunodeProtocol

/// 列表、状态条上给人看的文字。和界面框架无关，单元测试能直接测。
public enum Presentation {
    /// agent 的状态：名字加「干活中」「空闲」「等你回答」。没有 agent 或者状态不认识时为空。
    public static func agentStatus(_ agent: Agent?) -> (text: String, state: AgentState)? {
        guard let agent else { return nil }
        let state: String
        switch agent.state {
        case .working: state = "干活中"
        case .idle: state = "空闲"
        case .blocked: state = "等你回答"
        case .unknown: return nil
        }
        return ("\(agent.kind.displayName) · \(state)", agent.state)
    }

    /// 目录：家目录下的写成 `~/…`。
    public static func directory(_ path: String?) -> String? {
        guard let path, !path.isEmpty else { return nil }
        let parts = path.split(separator: "/", omittingEmptySubsequences: false)
        // /Users/<名字>/... 或 /home/<名字>/...
        if parts.count >= 3, parts[0].isEmpty, parts[1] == "Users" || parts[1] == "home" {
            let rest = parts.dropFirst(3).joined(separator: "/")
            return rest.isEmpty ? "~" : "~/\(rest)"
        }
        return path
    }

    public static func gridSize(_ size: GridSize) -> String {
        "\(size.cols)×\(size.rows)"
    }

    /// 会话列表里尺寸由谁控制。
    public static func sizeOwner(_ owner: String?) -> String {
        guard let owner, !owner.isEmpty else { return "尺寸无人控制" }
        return "尺寸跟随 \(owner)"
    }

    /// 终端页里尺寸由谁控制。
    public static func sizeOwnership(_ ownership: SizeOwnership) -> String {
        switch ownership {
        case .unknown: "尺寸跟随 Mac"
        case .mine: "尺寸跟随本机"
        case .other(let name?): "尺寸跟随 \(name)"
        case .other(nil): "尺寸跟随其他设备"
        }
    }

    public static func linkState(_ state: LinkState) -> String {
        switch state {
        case .idle: "未连接"
        case .connecting: "正在连接…"
        case .connected(let hostName, _): "已连接 \(hostName)"
        case .waiting(let reason, _): "\(reason)，稍后自动重连"
        case .failed(let failure): failure.errorDescription ?? "连接失败"
        }
    }

    public static func sessionTitle(_ session: SessionInfo) -> String {
        session.meta.displayTitle ?? "终端"
    }
}
