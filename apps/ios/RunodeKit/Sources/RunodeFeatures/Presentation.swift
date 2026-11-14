import Foundation
import RunodeConnection
import RunodeProtocol

/// 首页电脑卡片上的一行：开头 `emphasis` 一段上 `tone` 的状态色，后面接中性色的 `text`。
public struct MachineLine: Hashable, Sendable {
    public enum Tone: Hashable, Sendable {
        case waiting
        case working
        case failure
        case neutral
    }

    public var tone: Tone
    /// 上状态色的一段；`neutral` 时为空。
    public var emphasis: String?
    public var text: String

    public init(tone: Tone, emphasis: String? = nil, text: String) {
        self.tone = tone
        self.emphasis = emphasis
        self.text = text
    }
}

/// agent 状态旁的时长按 `step` 往下取整，不满一个 `step` 不写；从 `since` 起每满一个 `step` 重画一次。
enum AgentClock {
    static let step: TimeInterval = 60

    /// 从 `start` 起该重画的时刻，第一个是 `start` 本身，往后是 `since` 起的整步。
    static func ticks(since: Date, from start: Date) -> AnyIterator<Date> {
        var next = start
        return AnyIterator {
            defer { next = following(next, since: since) }
            return next
        }
    }

    /// `date` 之后的下一个整步；`date` 早于 `since` 时是 `since` 起的第一步。
    static func following(_ date: Date, since: Date) -> Date {
        let steps = (max(0, elapsed(since: since, now: date)) / step).rounded(.down) + 1
        return since.addingTimeInterval(steps * step)
    }

    /// `since` 到 `now` 的秒数，差一毫秒以内算正落在整步上：浮点误差会把重画时刻算到前一步，
    /// 时长就少显示一步，下一个重画时刻也还是它自己。
    static func elapsed(since: Date, now: Date) -> TimeInterval {
        now.timeIntervalSince(since) + 0.001
    }
}

/// 列表、状态条上给人看的文字。和界面框架无关，单元测试能直接测。
public enum Presentation {
    /// agent 的状态：在等和在干活时，`agentElapsed` 写得出时长就说等了、干了多久，写不出时说「等你回答」
    /// 「干活中」；空闲时说「空闲」。没有 agent 或者状态不认识时为空。`since` 是 `SessionMeta.agentSince`；
    /// 时长按 `locale` 写，不给时按 `interfaceLocale`。
    public static func agentStatus(
        _ agent: Agent?, since: Date?, now: Date, locale: Locale? = nil
    ) -> (text: String, state: AgentState)? {
        guard let agent else { return nil }
        let name = agent.kind.displayName
        let elapsed = agentElapsed(since: since, now: now, locale: locale ?? interfaceLocale)
        let state: String
        switch agent.state {
        case .working:
            if let elapsed { return (String(localized: "\(name) 干了 \(elapsed)"), agent.state) }
            state = String(localized: "干活中")
        case .idle: state = String(localized: "空闲")
        case .blocked:
            if let elapsed { return (String(localized: "\(name) 等了 \(elapsed)"), agent.state) }
            state = String(localized: "等你回答")
        case .unknown: return nil
        }
        return (String(localized: "\(name)，\(state)"), agent.state)
    }

    /// 从 `since` 到 `now` 多久，按 `locale` 以小时或分钟只写最大的一个单位、往下取整。没有 `since`，
    /// 或者不满一个 `AgentClock.step`（包括手机和电脑的钟有偏差、`since` 比 `now` 晚）时为空。
    static func agentElapsed(since: Date?, now: Date, locale: Locale = interfaceLocale) -> String? {
        guard let since else { return nil }
        let seconds = AgentClock.elapsed(since: since, now: now)
        guard seconds >= AgentClock.step else { return nil }
        let style = Duration.UnitsFormatStyle(
            allowedUnits: [.hours, .minutes], width: .wide, maximumUnitCount: 1, fractionalPart: .hide(rounded: .down)
        ).locale(locale)
        return Duration.seconds(seconds).formatted(style)
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

    /// 配对时列出的电脑地址：IPv4 在前，只写前两个，其余的只说还有几个；电脑的网卡多时整串地址又长又难认。
    static func pairingAddresses(_ addresses: [String], port: UInt16) -> String {
        let ordered = addresses.filter { !$0.contains(":") } + addresses.filter { $0.contains(":") }
        let shown = ordered.prefix(2).map { $0.contains(":") ? "[\($0)]:\(port)" : "\($0):\(port)" }
            .joined(separator: ", ")
        return ordered.count > 2 ? String(localized: "\(shown) 等 \(ordered.count) 个地址") : shown
    }

    /// 会话卡片上的目录：和所在工作区的目录一样时不写（节头已经写了），在它下面时写成 `./…`，
    /// 别处的照 `directory` 写。
    public static func sessionDirectory(_ cwd: String?, in workspaceDir: String?) -> String? {
        guard let cwd, !cwd.isEmpty else { return nil }
        guard let workspaceDir, !workspaceDir.isEmpty else { return directory(cwd) }
        let base = workspaceDir.hasSuffix("/") ? String(workspaceDir.dropLast()) : workspaceDir
        if cwd == base || cwd == base + "/" { return nil }
        if cwd.hasPrefix(base + "/") { return "./" + cwd.dropFirst(base.count + 1) }
        return directory(cwd)
    }

    public static func gridSize(_ size: GridSize) -> String {
        "\(size.cols)×\(size.rows)"
    }

    /// 会话列表里尺寸由谁控制。
    public static func sizeOwner(_ owner: String?) -> String {
        guard let owner, !owner.isEmpty else { return String(localized: "尺寸无人控制") }
        return String(localized: "尺寸跟随 \(owner)")
    }

    /// 终端页里尺寸由谁控制。
    public static func sizeOwnership(_ ownership: SizeOwnership) -> String {
        switch ownership {
        case .unknown: String(localized: "尺寸跟随电脑")
        case .mine: String(localized: "尺寸跟随本机")
        case .other(let name?): String(localized: "尺寸跟随 \(name)")
        case .other(nil): String(localized: "尺寸跟随其他设备")
        }
    }

    /// 连接状态的完整说法。`shownName` 是旁边已经写着的电脑名字：连上的那台报的名字和它一样时不再写一遍。
    public static func linkState(_ state: LinkState, shownName: String? = nil) -> String {
        switch state {
        case .idle: String(localized: "未连接")
        case .connecting: String(localized: "正在连接…")
        case .connected(let hostName, _) where hostName == shownName: String(localized: "已连接")
        case .connected(let hostName, _): String(localized: "已连接 \(hostName)")
        case .waiting(let reason, _, true): String(localized: "\(reason)，电脑上线后自动连接")
        case .waiting(let reason, _, false): String(localized: "\(reason)，稍后自动重连")
        case .failed(let failure): failure.errorDescription ?? String(localized: "连接失败")
        }
    }

    /// 标题下面一行短的连接状态：重连时带倒计时。
    public static func linkStatus(_ state: LinkState, now: Date = .now) -> String {
        switch state {
        case .idle: return String(localized: "未连接")
        case .connecting: return String(localized: "连接中…")
        case .connected: return String(localized: "已连接")
        case .waiting(_, _, true): return String(localized: "电脑离线，上线后自动连接")
        case .waiting(_, let retryAt, false):
            let seconds = Int(retryAt.timeIntervalSince(now).rounded(.up))
            return seconds > 0 ? String(localized: "已断开，\(seconds) 秒后重连") : String(localized: "已断开，正在重连")
        case .failed: return String(localized: "连接失败")
        }
    }

    /// 首页电脑卡片上的一行：连着时是会话数，有等你回答或在干活的先说它们；连不上时是「连不上」，
    /// 别的没连着的状态是连接状态。
    public static func machineLine(state: LinkState, sessions: [SessionInfo], loaded: Bool, now: Date = .now)
        -> MachineLine
    {
        switch state {
        case .connected: break
        case .failed: return MachineLine(tone: .failure, emphasis: String(localized: "连不上"), text: "")
        default: return MachineLine(tone: .neutral, text: linkStatus(state, now: now))
        }
        guard loaded else { return MachineLine(tone: .neutral, text: String(localized: "已连接")) }
        let live = sessions.filter { !$0.exited }
        guard !live.isEmpty else { return MachineLine(tone: .neutral, text: String(localized: "没有终端")) }
        let total = String(localized: "，共 \(live.count) 个会话")
        let waiting = live.filter { SessionGroup.of($0) == .waiting }.count
        if waiting > 0 {
            return MachineLine(tone: .waiting, emphasis: String(localized: "\(waiting) 个等你"), text: total)
        }
        let working = live.filter { SessionGroup.of($0) == .working }.count
        if working > 0 {
            return MachineLine(tone: .working, emphasis: String(localized: "\(working) 个在干活"), text: total)
        }
        return MachineLine(tone: .neutral, text: String(localized: "\(live.count) 个会话"))
    }

    /// 首页的大标题：有等你回答的说等你的个数，否则有在干活的说干活的个数。
    public static func homeTitle(_ summary: HomeSummary) -> String {
        if summary.waiting > 0 { return String(localized: "\(summary.waiting) 个会话在等你") }
        if summary.working > 0 { return String(localized: "\(summary.working) 个会话在干活") }
        if summary.connected > 0 { return String(localized: "没有会话在等你") }
        return String(localized: "还没连上电脑")
    }

    /// 首页大标题下面那行，标题已经说全了时为空。
    public static func homeDetail(_ summary: HomeSummary) -> String? {
        if summary.waiting > 0 {
            return summary.working > 0 ? String(localized: "另有 \(summary.working) 个在干活") : nil
        }
        if summary.working > 0 { return nil }
        if summary.sessions > 0 { return String(localized: "共 \(summary.sessions) 个会话") }
        if summary.connected > 0 { return String(localized: "没有终端") }
        return nil
    }

    /// 会话卡片上说状态的一句。给了 `machine`（会话所在电脑的名字）时，等你回答的那句带上它。在等和在干活时，
    /// `agentElapsed` 写得出时长就说等了、干了多久。时长按 `locale` 写，不给时按 `interfaceLocale`。
    public static func sessionSubtitle(
        _ session: SessionInfo, machine: String? = nil, now: Date, locale: Locale? = nil
    ) -> String {
        let agent = session.meta.agent?.kind.displayName ?? ""
        let elapsed = agentElapsed(since: session.meta.agentSince, now: now, locale: locale ?? interfaceLocale)
        switch SessionStatus.of(session) {
        case .waiting:
            switch (machine, elapsed) {
            case (let machine?, let elapsed?): return String(localized: "\(agent) 在 \(machine) 上等了 \(elapsed)")
            case (let machine?, nil): return String(localized: "\(agent) 在 \(machine) 上等你")
            case (nil, let elapsed?): return String(localized: "\(agent) 等了 \(elapsed)")
            case (nil, nil): return String(localized: "\(agent) 在等你")
            }
        case .working:
            if let elapsed { return String(localized: "\(agent) 干了 \(elapsed)") }
            return String(localized: "\(agent) 在干活")
        case .agentIdle: return String(localized: "\(agent) 空闲")
        case .running(let program): return String(localized: "在跑 \(program)")
        case .shell: return String(localized: "空闲")
        case .exited: return String(localized: "已退出")
        }
    }

    /// 会话列表标题下面一行：连着时说经哪条路连的，没连着时是连接状态。没有地址的连接走的是跨网络的隧道。
    public static func linkRoute(_ state: LinkState, now: Date = .now) -> String {
        guard case .connected(_, let address) = state else { return linkStatus(state, now: now) }
        guard let address else { return String(localized: "已连接，跨网络") }
        return isLocalAddress(address) ? String(localized: "已连接，局域网直连") : String(localized: "已连接，直连")
    }

    /// 是不是局域网里的地址：IPv4 私网和链路本地、IPv6 链路本地和唯一本地，以及 mDNS 的 `.local` 名字。
    /// 地址后面的网卡作用域（`%en0`）不影响判断。
    static func isLocalAddress(_ address: String) -> Bool {
        var bare = (address.firstIndex(of: "%").map { String(address[..<$0]) } ?? address).lowercased()
        if bare.hasSuffix(".") { bare.removeLast() }
        if bare.hasSuffix(".local") { return true }
        // IPv4 映射的 IPv6 地址按里面的 IPv4 判断。
        if bare.hasPrefix("::ffff:"), bare.contains(".") { bare.removeFirst("::ffff:".count) }
        if bare.contains(":") {
            guard let first = bare.split(separator: ":", omittingEmptySubsequences: false).first,
                let head = UInt16(first, radix: 16)
            else { return false }
            return head & 0xffc0 == 0xfe80 || head & 0xfe00 == 0xfc00
        }
        let octets = bare.split(separator: ".", omittingEmptySubsequences: false).map { UInt8($0) }
        guard octets.count == 4, let a = octets[0], let b = octets[1], octets.allSatisfy({ $0 != nil }) else {
            return false
        }
        return a == 10 || (a == 172 && (16...31).contains(b)) || (a == 192 && b == 168) || (a == 169 && b == 254)
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

    /// 会话列表里一节的标题：工作区的名字，没有名字时是目录的最后一段；不在任何窗口里的是「后台」。
    public static func sectionTitle(_ section: SessionSection) -> String {
        switch section.id {
        case .background: return String(localized: "后台")
        case .workspace(_, let index):
            if let name = section.name, !name.isEmpty { return name }
            if let dir = section.dir, !dir.isEmpty { return (dir as NSString).lastPathComponent }
            return String(localized: "工作区 \(index)")
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
            return String(localized: "电脑上没有窗口在显示这些终端")
        case .workspace:
            switch (sectionWindow(section), sectionDirectory(section)) {
            case let (window?, directory?): return String(localized: "\(window)，\(directory)")
            case let (window, directory): return window ?? directory
            }
        }
    }

    /// 工作区的目录；后台那一节，或者目录写出来和标题一样时（家目录的工作区叫 `~`）为空。
    public static func sectionDirectory(_ section: SessionSection) -> String? {
        guard case .workspace = section.id else { return nil }
        return directory(section.dir).flatMap { $0 == sectionTitle(section) ? nil : $0 }
    }

    /// 工作区在第几个窗口；只开着一个窗口时，或者后台那一节为空。
    public static func sectionWindow(_ section: SessionSection) -> String? {
        guard case .workspace = section.id else { return nil }
        return section.window.map { String(localized: "窗口 \($0)") }
    }

    /// 终端页标题下面那行：shell 退出或会话没了时说这个，没连上时是连接状态，否则是电脑的名字。
    public static func terminalSubtitle(machine: String?, link: LinkState, phase: TerminalModel.Phase, now: Date = .now)
        -> String
    {
        switch phase {
        case .exited: return String(localized: "shell 已退出")
        case .gone: return String(localized: "会话已结束")
        default:
            guard link.isConnected, let machine else { return linkStatus(link, now: now) }
            return machine
        }
    }

    public static func sizePreference(_ preference: SizePreference) -> String {
        switch preference {
        case .automatic: String(localized: "自动")
        case .fitPhone: String(localized: "适配手机")
        case .followMachine: String(localized: "跟随电脑")
        }
    }

    /// agent 的 logo 在资源里的名字，和桌面上 `agent_logo` 用的是同一批 SVG；没收 logo 的为空。
    public static func agentLogoAsset(_ kind: AgentKind) -> String? {
        let assets: [String: String] = [
            "pi": "pi", "claude": "claude", "codex": "codex", "gemini": "gemini", "cursor": "cursor",
            "devin": "devin", "antigravity": "antigravity", "cline": "cline", "mastracode": "mastracode",
            "open_code": "opencode", "github_copilot": "github-copilot", "kimi": "kimi", "kiro": "kiro", "amp": "amp",
            "grok": "grok", "hermes": "hermes", "kilo": "kilo", "qodercli": "qoder", "qwen": "qwen", "goose": "goose",
            "junie": "junie", "open_hands": "openhands", "trae": "trae", "code_buddy": "codebuddy",
            "mistral_vibe": "mistral-vibe", "jules": "jules", "omp": "omp",
        ]
        return assets[kind.label]
    }

    public static func sessionTitle(_ session: SessionInfo) -> String {
        session.meta.displayTitle ?? String(localized: "终端")
    }

    /// 会话卡片菜单里一组命令的名字：文件名，不在会话目录里时带上它在哪一级（`Makefile，../..`）。
    /// 自己加的命令按这个项目的和通用的分。
    public static func taskSourceTitle(_ source: TaskSource, cwd: String?) -> String {
        switch source.kind {
        case .custom: return String(localized: "我的命令")
        case .global: return String(localized: "通用命令")
        default: break
        }
        let file = (source.file as NSString).lastPathComponent
        let dir = (source.file as NSString).deletingLastPathComponent
        guard let cwd, cwd != dir, cwd.hasPrefix(dir.hasSuffix("/") ? dir : dir + "/") else { return file }
        let levels = cwd.dropFirst(dir.count).split(separator: "/").count
        let up = Array(repeating: "..", count: levels).joined(separator: "/")
        return String(localized: "\(file)，\(up)")
    }

    public static func taskSourceSymbol(_ source: TaskSource) -> String {
        switch source.kind {
        case .custom: "star"
        case .global: "globe"
        case .makefile: "hammer"
        case .packageJson: "shippingbox"
        case .unknown: "terminal"
        }
    }

    /// 界面上写的程序名：Windows 上的进程名带着 `.exe`，写成 `powershell` 而不是 `powershell.exe`。
    static func programLabel(_ name: String) -> String {
        guard name.count > 4, name.lowercased().hasSuffix(".exe") else { return name }
        return String(name.dropLast(4))
    }

    /// 菜单里命令那一节的标题：终端结束了时说明为什么点不了，前台在跑别的程序时说明命令会在新终端里跑。
    public static func tasksHeader(_ session: SessionInfo) -> String {
        if session.exited { return String(localized: "运行，终端已经结束") }
        if session.meta.foregroundIsShell { return String(localized: "运行") }
        if let foreground = session.meta.foreground, !foreground.isEmpty {
            return String(localized: "运行，前台在跑 \(programLabel(foreground))，在新终端里跑")
        }
        return String(localized: "运行，在新终端里跑")
    }
}
