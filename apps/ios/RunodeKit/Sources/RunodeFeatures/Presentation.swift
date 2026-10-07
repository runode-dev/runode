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
        case .unknown: "尺寸跟随电脑"
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

    /// 首页电脑卡片上的一行：连着时是会话数和在干活、等回答的个数（前面的绿点已经说明连着，不再写
    /// 「已连接」），没连着时是连接状态。
    public static func machineSummary(_ state: LinkState, sessions: [SessionInfo], loaded: Bool, now: Date = .now)
        -> String
    {
        guard state.isConnected else { return linkStatus(state, now: now) }
        guard loaded else { return "已连接" }
        let live = sessions.filter { !$0.exited }
        guard !live.isEmpty else { return "没有终端" }
        var parts = ["\(live.count) 个会话"]
        let waiting = live.filter { SessionGroup.of($0) == .waiting }.count
        let working = live.filter { SessionGroup.of($0) == .working }.count
        if waiting > 0 { parts.append("\(waiting) 个等你回答") }
        if working > 0 { parts.append("\(working) 个在干活") }
        return parts.joined(separator: " · ")
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

    /// 会话列表里一节的标题：工作区的名字，没有名字时是目录的最后一段；不在任何窗口里的是「后台」。
    public static func sectionTitle(_ section: SessionSection) -> String {
        switch section.id {
        case .background: return "后台"
        case .workspace(_, let index):
            if let name = section.name, !name.isEmpty { return name }
            if let dir = section.dir, !dir.isEmpty { return (dir as NSString).lastPathComponent }
            return "工作区 \(index)"
        }
    }

    public static func sectionSymbol(_ section: SessionSection) -> String {
        switch section.id {
        case .background: "moon.zzz"
        case .workspace: "folder"
        }
    }

    /// 一节标题下面那行小字：工作区的目录，开着不止一个窗口时带上第几个窗口；后台那一节说明它是什么。
    public static func sectionDetail(_ section: SessionSection) -> String? {
        switch section.id {
        case .background:
            return "电脑上没有窗口在显示这些终端"
        case .workspace:
            let parts = [section.window.map { "窗口 \($0)" }, directory(section.dir)].compactMap { $0 }
            return parts.isEmpty ? nil : parts.joined(separator: " · ")
        }
    }

    /// 终端页标题下面那行：agent 状态和连接状态。有 agent 状态又连得好好的时不写「已连接」，只在
    /// 连接出了状况时才占地方。
    public static func terminalSubtitle(agent: Agent?, link: LinkState, phase: TerminalModel.Phase, now: Date = .now)
        -> String
    {
        var parts: [String] = []
        if let status = agentStatus(agent) { parts.append(status.text) }
        switch phase {
        case .exited: parts.append("shell 已退出")
        case .gone: parts.append("会话已结束")
        default:
            if parts.isEmpty || !link.isConnected { parts.append(linkStatus(link, now: now)) }
        }
        return parts.joined(separator: " · ")
    }

    public static func sizePreference(_ preference: SizePreference) -> String {
        switch preference {
        case .automatic: "自动"
        case .fitPhone: "适配手机"
        case .followMachine: "跟随电脑"
        }
    }

    public static func sessionTitle(_ session: SessionInfo) -> String {
        session.meta.displayTitle ?? "终端"
    }

    /// 会话卡片菜单里一组项目命令的名字：文件名，不在会话目录里时带上它在哪一级（`Makefile · ../..`）。
    public static func taskSourceTitle(_ source: TaskSource, cwd: String?) -> String {
        let file = (source.file as NSString).lastPathComponent
        let dir = (source.file as NSString).deletingLastPathComponent
        guard let cwd, cwd != dir, cwd.hasPrefix(dir.hasSuffix("/") ? dir : dir + "/") else { return file }
        let levels = cwd.dropFirst(dir.count).split(separator: "/").count
        return "\(file) · \(Array(repeating: "..", count: levels).joined(separator: "/"))"
    }

    public static func taskSourceSymbol(_ source: TaskSource) -> String {
        switch source.kind {
        case .makefile: "hammer"
        case .packageJson: "shippingbox"
        case .unknown: "terminal"
        }
    }

    /// 菜单里项目命令那一节的标题：终端结束了时说明为什么点不了，前台在跑别的程序时说明命令会在新终端里跑。
    public static func projectTasksHeader(_ session: SessionInfo) -> String {
        if session.exited { return "运行 · 终端已经结束" }
        if session.meta.foregroundIsShell { return "运行" }
        if let foreground = session.meta.foreground, !foreground.isEmpty {
            return "运行 · 前台在跑 \(foreground)，在新终端里跑"
        }
        return "运行 · 在新终端里跑"
    }
}
