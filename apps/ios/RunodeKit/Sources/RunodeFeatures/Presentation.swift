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

    /// 标题下面一行短的连接状态：重连时带倒计时。
    public static func linkStatus(_ state: LinkState, now: Date = .now) -> String {
        switch state {
        case .idle: return "未连接"
        case .connecting: return "连接中…"
        case .connected: return "已连接"
        case .waiting(_, let retryAt):
            let seconds = Int(retryAt.timeIntervalSince(now).rounded(.up))
            return seconds > 0 ? "已断开，\(seconds) 秒后重连" : "已断开，正在重连"
        case .failed: return "连接失败"
        }
    }

    /// 给人看的地址：去掉 IPv4/IPv6 链路本地地址后面的网卡作用域（`%en0`）。连接用的地址本身不改。
    public static func displayAddress(_ address: String) -> String {
        guard let percent = address.firstIndex(of: "%") else { return address }
        return String(address[..<percent])
    }

    /// 屏幕文字的最后几行：去掉行尾空白、空行和只有制表符（分隔线、边框）的行。`atPrompt` 为真
    /// （shell 在提示符上等输入）时再去掉最后一个有字的行，那就是光标所在的提示符，信息量很低。
    public static func previewLines(_ text: String, limit: Int, atPrompt: Bool = false) -> [String] {
        let lines = text.split(separator: "\n", omittingEmptySubsequences: false).map { line in
            var line = String(line)
            while let last = line.last, last.isWhitespace { line.removeLast() }
            return line
        }
        let meaningful = lines.filter { line in
            line.unicodeScalars.contains { scalar in
                !scalar.properties.isWhitespace && !(0x2500...0x257F).contains(scalar.value)
            }
        }
        return Array((atPrompt ? meaningful.dropLast() : meaningful[...]).suffix(limit))
    }

    /// 分组和 agent 状态用的 SF Symbol：颜色之外还要靠图标区分。
    public static func symbol(for group: SessionGroup) -> String {
        switch group {
        case .waiting: "exclamationmark.bubble.fill"
        case .working: "gearshape.2.fill"
        case .other: "terminal"
        }
    }

    public static func title(for group: SessionGroup) -> String {
        switch group {
        case .waiting: "等你回答"
        case .working: "干活中"
        case .other: "其他会话"
        }
    }

    /// 终端页标题下面那行：agent 状态和连接状态。
    public static func terminalSubtitle(agent: Agent?, link: LinkState, phase: TerminalModel.Phase, now: Date = .now)
        -> String
    {
        var parts: [String] = []
        if let status = agentStatus(agent) { parts.append(status.text) }
        switch phase {
        case .exited: parts.append("shell 已退出")
        case .gone: parts.append("会话已结束")
        default: parts.append(linkStatus(link, now: now))
        }
        return parts.joined(separator: " · ")
    }

    public static func sizePreference(_ preference: SizePreference) -> String {
        switch preference {
        case .automatic: "自动"
        case .fitPhone: "适配手机"
        case .followMac: "跟随 Mac"
        }
    }

    public static func sessionTitle(_ session: SessionInfo) -> String {
        session.meta.displayTitle ?? "终端"
    }
}
