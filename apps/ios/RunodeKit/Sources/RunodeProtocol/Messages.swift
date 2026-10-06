import Foundation

/// 宿主协议的版本，和宿主的 `PROTOCOL_VERSION` 一致。宿主在 `Hello` 里比对，对不上回 `Incompatible`。
public let protocolVersion: UInt32 = 4

/// 连上会话时要什么，见宿主的 `AttachMode`。
public enum AttachMode: String, Hashable, Sendable, Codable {
    /// 同一个构建才解得了的快照；iOS 的构建和宿主不同，用不上。
    case snapshot
    /// 重画当前屏幕的 VT 序列，喂给一份新的 VT。
    case vtReplay = "vt_replay"
    /// 不要屏幕，只要 `Meta` 这些状态。
    case metaOnly = "meta_only"
}

/// `Open` 的新终端放在桌面 app 里的哪里，见宿主的 `Placement`。
public enum Placement: String, Hashable, Sendable, Codable {
    /// 紧跟在旁边那个终端的标签后面的新标签。
    case tab
    /// 把旁边那个终端一分为二，新终端在右边。
    case right
    /// 把旁边那个终端一分为二，新终端在下边。
    case down
}

/// 前端能做什么，缺的项按不能。
public struct Caps: Hashable, Sendable, Codable {
    public var snapshot: Bool
    public var vtReplay: Bool

    public init(snapshot: Bool, vtReplay: Bool) {
        self.snapshot = snapshot
        self.vtReplay = vtReplay
    }

    enum CodingKeys: String, CodingKey {
        case snapshot
        case vtReplay = "vt_replay"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        snapshot = try c.decodeIfPresent(Bool.self, forKey: .snapshot) ?? false
        vtReplay = try c.decodeIfPresent(Bool.self, forKey: .vtReplay) ?? false
    }
}

/// 前端的种类。比自己新的一方才有的种类读成 `unknown`。
public enum ClientKind: Hashable, Sendable, Codable {
    case desktop, cli, tui, mobile, successor
    case unknown(String)

    var wireName: String {
        switch self {
        case .desktop: "desktop"
        case .cli: "cli"
        case .tui: "tui"
        case .mobile: "mobile"
        case .successor: "successor"
        case .unknown(let name): name
        }
    }

    public init(from decoder: any Decoder) throws {
        let text = try decoder.singleValueContainer().decode(String.self)
        self =
            [ClientKind.desktop, .cli, .tui, .mobile, .successor].first { $0.wireName == text } ?? .unknown(text)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(wireName)
    }
}

/// 前端发给宿主的消息。只列出手机这个前端会发的几种：`Hello`、`ListSessions`、`Layout`、`Open`、
/// `OpenWorkspace`、`ListDirs`、`Spawn`、`Attach`、`Detach`、`Resize`、`Focus`、`ClearScreen`、`Kill`、
/// `ReadScreen`、`SendKeys`、`Paste`、`Git`。JSON 的样子和
/// 宿主的 `ClientMsg` 一致，可缺省的字段也照宿主序列化的样子写出 `null`。宿主对手机连接上的 `Shutdown`、
/// 交接（`Handoff` 等）、`UiReply`、`SetOptions`、`SetTheme` 只回 `Error`，这里故意不定义它们，手机就
/// 发不出去。
public enum ClientMsg: Hashable, Sendable, Encodable {
    /// 连上后的第一条消息。手机的 `client` 是 `mobile`，`caps` 只要 VT 重放。
    case hello(protocol: UInt32, build: String, client: ClientKind, caps: Caps, session: SessionId?, device: String?)
    case listSessions
    /// 要电脑上的 app 里各个终端摆在哪，宿主转给桌面的界面，回 `Layout`；没有桌面的界面时回 `Error`。
    case layout(req: UInt32)
    /// 请电脑上的 app 在窗口里开一个新终端，宿主转给桌面的界面，回 `Opened`；没有桌面窗口时回
    /// `Error`。`near` 为空时放在最前面那个窗口当前的分屏旁边，`cwd` 为空时沿用旁边那个终端的目录，
    /// `focus` 为假时不切过去。
    case open(req: UInt32, placement: Placement, near: SessionId?, cwd: String?, focus: Bool)
    /// 请电脑上的 app 在最前面那个窗口里新建一个目录是 `dir`（绝对路径）的工作区，回 `Opened`，带着它的
    /// 第一个终端；已经有这个目录的工作区时回那个工作区当前的终端。`focus` 为假时电脑上不切过去。
    case openWorkspace(req: UInt32, dir: String, focus: Bool)
    /// 列电脑上一个目录（绝对路径）里的子目录，为空时是家目录，宿主回 `Dirs`，出错时回 `Error`。
    case listDirs(req: UInt32, path: String?)
    /// 新开一个会话；宿主回 `Spawned`，开好的会话要另外 `Attach`。
    case spawn(req: UInt32, size: GridSize, cwd: String?, integration: IntegrationMode, start: Bool)
    /// 连上会话。`size` 为空时不改会话的尺寸，也不算一次交互（见宿主的尺寸归属）。
    case attach(id: SessionId, size: GridSize?, mode: AttachMode)
    case detach(id: SessionId)
    /// 视图的尺寸变了：宿主改好后在输出流里标出 `Resized`。
    case resize(id: SessionId, size: GridSize)
    /// 这个会话在前端被看着；`focused` 为真算一次交互，可能轮到这条连接决定尺寸。
    case focus(id: SessionId, focused: Bool)
    case clearScreen(id: SessionId)
    /// 结束会话。
    case kill(id: SessionId)
    /// 读会话屏幕底部的文字：从最后一个有字的行往上 `lines` 行（含回滚历史），为空时是当前一屏。
    /// `command` 为 `n` 时不看 `lines`，读倒数第 n 条命令（1 是最近一条）的输出，不含提示符，要 shell
    /// 集成标出的提示符。宿主回 `ScreenText`，出错时回带着会话标识的 `Error`。不用连上会话。
    case readScreen(id: SessionId, lines: UInt32?, command: UInt32? = nil)
    /// 发控制键（`enter`、`esc`、`up`、`1` 这类写法），宿主按它那份 VT 当前的模式编码后写进去，回
    /// `Done`。不用连上会话。
    case sendKeys(req: UInt32, id: SessionId, keys: [String])
    /// 粘贴一段文字，程序开着括号粘贴时宿主套上括号，回 `Done`。不用连上会话。
    case paste(req: UInt32, id: SessionId, text: String)
    /// 在会话 shell 当前所在的仓库里读写 git，回 `GitStatus`、`GitDiff` 或 `GitBranches`；办不了时回
    /// 只带 `req`、不带会话 `id` 的 `Error`。一条连接上的这些请求宿主按先后一件一件办。
    case git(req: UInt32, id: SessionId, request: GitRequest)

    private struct Key: CodingKey {
        var stringValue: String
        var intValue: Int? { nil }
        init(_ string: String) { stringValue = string }
        init?(stringValue: String) { self.stringValue = stringValue }
        init?(intValue: Int) { nil }
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: Key.self)
        switch self {
        case let .hello(protocolVersion, build, client, caps, session, device):
            try c.encode("hello", forKey: Key("type"))
            try c.encode(protocolVersion, forKey: Key("protocol"))
            try c.encode(build, forKey: Key("build"))
            try c.encode(client, forKey: Key("client"))
            try c.encode(caps, forKey: Key("caps"))
            try c.encode(session, forKey: Key("session"))
            try c.encode(device, forKey: Key("device"))
        case .listSessions:
            try c.encode("list_sessions", forKey: Key("type"))
        case let .layout(req):
            try c.encode("layout", forKey: Key("type"))
            try c.encode(req, forKey: Key("req"))
        case let .open(req, placement, near, cwd, focus):
            try c.encode("open", forKey: Key("type"))
            try c.encode(req, forKey: Key("req"))
            try c.encode(placement, forKey: Key("placement"))
            try c.encode(near, forKey: Key("near"))
            try c.encode(cwd, forKey: Key("cwd"))
            try c.encode(focus, forKey: Key("focus"))
        case let .openWorkspace(req, dir, focus):
            try c.encode("open_workspace", forKey: Key("type"))
            try c.encode(req, forKey: Key("req"))
            try c.encode(dir, forKey: Key("dir"))
            try c.encode(focus, forKey: Key("focus"))
        case let .listDirs(req, path):
            try c.encode("list_dirs", forKey: Key("type"))
            try c.encode(req, forKey: Key("req"))
            try c.encode(path, forKey: Key("path"))
        case let .spawn(req, size, cwd, integration, start):
            try c.encode("spawn", forKey: Key("type"))
            try c.encode(req, forKey: Key("req"))
            try c.encode(size, forKey: Key("size"))
            try c.encode(cwd, forKey: Key("cwd"))
            try c.encode(integration, forKey: Key("integration"))
            try c.encode(start, forKey: Key("start"))
            try c.encodeNil(forKey: Key("shell"))
            try c.encodeNil(forKey: Key("settings"))
            try c.encode([[String]](), forKey: Key("env"))
        case let .attach(id, size, mode):
            try c.encode("attach", forKey: Key("type"))
            try c.encode(id, forKey: Key("id"))
            try c.encode(size, forKey: Key("size"))
            try c.encode(mode, forKey: Key("mode"))
        case let .detach(id):
            try c.encode("detach", forKey: Key("type"))
            try c.encode(id, forKey: Key("id"))
        case let .resize(id, size):
            try c.encode("resize", forKey: Key("type"))
            try c.encode(id, forKey: Key("id"))
            try c.encode(size, forKey: Key("size"))
        case let .focus(id, focused):
            try c.encode("focus", forKey: Key("type"))
            try c.encode(id, forKey: Key("id"))
            try c.encode(focused, forKey: Key("focused"))
        case let .clearScreen(id):
            try c.encode("clear_screen", forKey: Key("type"))
            try c.encode(id, forKey: Key("id"))
        case let .kill(id):
            try c.encode("kill", forKey: Key("type"))
            try c.encode(id, forKey: Key("id"))
        case let .readScreen(id, lines, command):
            try c.encode("read_screen", forKey: Key("type"))
            try c.encode(id, forKey: Key("id"))
            try c.encode(lines, forKey: Key("lines"))
            try c.encode(command, forKey: Key("command"))
        case let .sendKeys(req, id, keys):
            try c.encode("send_keys", forKey: Key("type"))
            try c.encode(req, forKey: Key("req"))
            try c.encode(id, forKey: Key("id"))
            try c.encode(keys, forKey: Key("keys"))
        case let .paste(req, id, text):
            try c.encode("paste", forKey: Key("type"))
            try c.encode(req, forKey: Key("req"))
            try c.encode(id, forKey: Key("id"))
            try c.encode(text, forKey: Key("text"))
        case let .git(req, id, request):
            try c.encode("git", forKey: Key("type"))
            try c.encode(req, forKey: Key("req"))
            try c.encode(id, forKey: Key("id"))
            try c.encode(request, forKey: Key("request"))
        }
    }
}

/// `SessionList` 里的一个会话。
public struct SessionInfo: Hashable, Sendable, Decodable {
    public var id: SessionId
    public var size: GridSize
    public var meta: SessionMeta
    /// 现在连着它的前端有几个。
    public var clients: UInt32
    /// 有桌面的界面连着它。
    public var claimed: Bool
    /// shell 已经退出，会话还留着。
    public var exited: Bool
    /// 现在决定这个会话尺寸的前端的设备名；没有 owner 或者它没报设备名时为空。
    public var sizeOwner: String?

    public init(
        id: SessionId, size: GridSize, meta: SessionMeta, clients: UInt32 = 0, claimed: Bool = false,
        exited: Bool = false, sizeOwner: String? = nil
    ) {
        self.id = id
        self.size = size
        self.meta = meta
        self.clients = clients
        self.claimed = claimed
        self.exited = exited
        self.sizeOwner = sizeOwner
    }

    enum CodingKeys: String, CodingKey {
        case id, size, meta, clients, claimed, exited
        case sizeOwner = "size_owner"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(SessionId.self, forKey: .id)
        size = try c.decode(GridSize.self, forKey: .size)
        meta = try c.decode(SessionMeta.self, forKey: .meta)
        clients = try c.decodeIfPresent(UInt32.self, forKey: .clients) ?? 0
        claimed = try c.decodeIfPresent(Bool.self, forKey: .claimed) ?? false
        exited = try c.decodeIfPresent(Bool.self, forKey: .exited) ?? false
        sizeOwner = try c.decodeIfPresent(String.self, forKey: .sizeOwner)
    }
}

/// shell 集成报告运行完的一条命令。
public struct FinishedCommand: Hashable, Sendable, Decodable {
    public var cmd: String
    public var cwd: String?
    public var exit: Int32?
    public var ts: UInt64
}

/// 宿主为什么断开。比自己新的宿主才有的原因读成 `unknown`。
public enum GoodbyeReason: Hashable, Sendable, Decodable {
    case shutdown
    /// 交接给了新版本的宿主，重新连上就是新宿主。
    case handoff
    case idle
    case error(String)
    case unknown(String)

    private enum Keys: String, CodingKey { case kind, message }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        let kind = try c.decode(String.self, forKey: .kind)
        switch kind {
        case "shutdown": self = .shutdown
        case "handoff": self = .handoff
        case "idle": self = .idle
        case "error": self = .error(try c.decodeIfPresent(String.self, forKey: .message) ?? "")
        default: self = .unknown(kind)
        }
    }
}

/// 宿主发给前端的消息，和宿主的 `HostMsg` 一一对应。不认识的种类读成 `unknown`（和宿主的
/// `#[serde(other)]` 一样），认识的种类缺了必填字段时整条解析失败。手机用不上的几种（界面转来的
/// 请求、交接）只解出编号，内容不读。
public enum HostMsg: Hashable, Sendable, Decodable {
    case welcome(protocol: UInt32, build: String, hostPid: UInt32, snapshotFormat: UInt16, standalone: Bool)
    case incompatible(protocol: UInt32, build: String, reason: String)
    case sessionList([SessionInfo])
    case spawned(req: UInt32, id: SessionId)
    case attached(Attached)
    case snapshotEnd(id: SessionId)
    case resized(id: SessionId, size: GridSize)
    case themeApplied(id: SessionId, settings: TermSettings)
    case meta(id: SessionId, meta: SessionMeta)
    case commandFinished(id: SessionId, command: FinishedCommand)
    case resync(id: SessionId, reason: String)
    case exited(id: SessionId, status: Int32?)
    case bell(id: SessionId)
    case opened(req: UInt32, id: SessionId)
    case done(req: UInt32)
    case screenText(id: SessionId, text: String, truncated: Bool)
    /// 回 `Layout`。
    case layout(req: UInt32, windows: [WindowLayout])
    /// 回 `ListDirs`：实际列的目录（规范化后的绝对路径）和它的子目录名，按名字排好；`truncated` 为真时
    /// 子目录太多，只给了前面一部分。
    case dirs(req: UInt32, path: String, dirs: [String], truncated: Bool)
    /// 回 `Git` 里读状态和改仓库的操作：办完以后仓库的样子；会话的目录不在 git 仓库里时为空。
    case gitStatus(req: UInt32, id: SessionId, status: GitStatus?)
    /// 回 `GitRequest.diff`；这个文件在那一段里已经没有改动时为空。
    case gitDiff(req: UInt32, id: SessionId, diff: GitFileDiff?)
    /// 回 `GitRequest.branches`：本地分支在前，远端分支在后，各按最近一次提交的时间倒序。
    case gitBranches(req: UInt32, id: SessionId, branches: [GitBranch])
    case uiRequest(ui: UInt64)
    case error(req: UInt32?, id: SessionId?, message: String)
    case goodbye(GoodbyeReason)
    case handoffRefused
    case handoffBegin(format: UInt32, sessions: UInt32)
    case sizeOwner(id: SessionId, mine: Bool, owner: String?)
    /// 比自己新的宿主才有的消息，忽略。
    case unknown(String)

    /// 回 `Attach`。之后先是快照帧和 `SnapshotEnd`（`metaOnly` 时没有），再是输出。
    public struct Attached: Hashable, Sendable {
        public var id: SessionId
        /// 这个会话的帧在这条连接上用的通道。
        public var channel: UInt32
        public var size: GridSize
        /// 宿主实际给的屏幕。
        public var mode: AttachMode
        public var meta: SessionMeta
        /// 宿主那份 VT 现在套着的主题；VT 重放时按它和 `size` 新建自己的 VT。
        public var settings: TermSettings?

        public init(
            id: SessionId, channel: UInt32, size: GridSize, mode: AttachMode, meta: SessionMeta,
            settings: TermSettings?
        ) {
            self.id = id
            self.channel = channel
            self.size = size
            self.mode = mode
            self.meta = meta
            self.settings = settings
        }
    }

    /// 宿主收到不认识的消息时回的 `Error` 的说明（不带请求编号）。手机发了比电脑上的 runode 新的消息时
    /// 据此知道它太旧。
    public static let unknownMessage = "unknown message"

    /// 这条消息说的是哪个会话；不针对某个会话的消息为空。
    public var sessionId: SessionId? {
        switch self {
        case .attached(let attached): attached.id
        case .snapshotEnd(let id), .resized(let id, _), .themeApplied(let id, _), .meta(let id, _),
            .commandFinished(let id, _), .resync(let id, _), .exited(let id, _), .bell(let id),
            .screenText(let id, _, _), .sizeOwner(let id, _, _), .spawned(_, let id), .opened(_, let id):
            id
        case .error(_, let id, _): id
        default: nil
        }
    }

    private enum Keys: String, CodingKey {
        case type, `protocol`, build, reason, sessions, req, id, channel, size, mode, meta, settings, command
        case status, text, truncated, ui, message, format, mine, owner, standalone, windows, path, dirs, diff, branches
        case hostPid = "host_pid"
        case snapshotFormat = "snapshot_format"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        let type = try c.decode(String.self, forKey: .type)
        switch type {
        case "welcome":
            self = .welcome(
                protocol: try c.decode(UInt32.self, forKey: .protocol),
                build: try c.decode(String.self, forKey: .build),
                hostPid: try c.decode(UInt32.self, forKey: .hostPid),
                snapshotFormat: try c.decode(UInt16.self, forKey: .snapshotFormat),
                standalone: try c.decodeIfPresent(Bool.self, forKey: .standalone) ?? false)
        case "incompatible":
            self = .incompatible(
                protocol: try c.decode(UInt32.self, forKey: .protocol),
                build: try c.decode(String.self, forKey: .build),
                reason: try c.decode(String.self, forKey: .reason))
        case "session_list":
            self = .sessionList(try c.decode([SessionInfo].self, forKey: .sessions))
        case "spawned":
            self = .spawned(req: try c.decode(UInt32.self, forKey: .req), id: try c.decode(SessionId.self, forKey: .id))
        case "attached":
            self = .attached(
                Attached(
                    id: try c.decode(SessionId.self, forKey: .id),
                    channel: try c.decode(UInt32.self, forKey: .channel),
                    size: try c.decode(GridSize.self, forKey: .size),
                    mode: try c.decode(AttachMode.self, forKey: .mode),
                    meta: try c.decode(SessionMeta.self, forKey: .meta),
                    settings: try c.decodeIfPresent(TermSettings.self, forKey: .settings)))
        case "snapshot_end":
            self = .snapshotEnd(id: try c.decode(SessionId.self, forKey: .id))
        case "resized":
            self = .resized(id: try c.decode(SessionId.self, forKey: .id), size: try c.decode(GridSize.self, forKey: .size))
        case "theme_applied":
            self = .themeApplied(
                id: try c.decode(SessionId.self, forKey: .id),
                settings: try c.decode(TermSettings.self, forKey: .settings))
        case "meta":
            self = .meta(id: try c.decode(SessionId.self, forKey: .id), meta: try c.decode(SessionMeta.self, forKey: .meta))
        case "command_finished":
            self = .commandFinished(
                id: try c.decode(SessionId.self, forKey: .id),
                command: try c.decode(FinishedCommand.self, forKey: .command))
        case "resync":
            self = .resync(id: try c.decode(SessionId.self, forKey: .id), reason: try c.decode(String.self, forKey: .reason))
        case "exited":
            self = .exited(
                id: try c.decode(SessionId.self, forKey: .id), status: try c.decodeIfPresent(Int32.self, forKey: .status))
        case "bell":
            self = .bell(id: try c.decode(SessionId.self, forKey: .id))
        case "opened":
            self = .opened(req: try c.decode(UInt32.self, forKey: .req), id: try c.decode(SessionId.self, forKey: .id))
        case "done":
            self = .done(req: try c.decode(UInt32.self, forKey: .req))
        case "screen_text":
            self = .screenText(
                id: try c.decode(SessionId.self, forKey: .id),
                text: try c.decode(String.self, forKey: .text),
                truncated: try c.decodeIfPresent(Bool.self, forKey: .truncated) ?? false)
        case "layout":
            self = .layout(
                req: try c.decode(UInt32.self, forKey: .req),
                windows: try c.decode([WindowLayout].self, forKey: .windows))
        case "dirs":
            self = .dirs(
                req: try c.decode(UInt32.self, forKey: .req),
                path: try c.decode(String.self, forKey: .path),
                dirs: try c.decode([String].self, forKey: .dirs),
                truncated: try c.decodeIfPresent(Bool.self, forKey: .truncated) ?? false)
        case "git_status":
            self = .gitStatus(
                req: try c.decode(UInt32.self, forKey: .req),
                id: try c.decode(SessionId.self, forKey: .id),
                status: try c.decodeIfPresent(GitStatus.self, forKey: .status))
        case "git_diff":
            self = .gitDiff(
                req: try c.decode(UInt32.self, forKey: .req),
                id: try c.decode(SessionId.self, forKey: .id),
                diff: try c.decodeIfPresent(GitFileDiff.self, forKey: .diff))
        case "git_branches":
            self = .gitBranches(
                req: try c.decode(UInt32.self, forKey: .req),
                id: try c.decode(SessionId.self, forKey: .id),
                branches: try c.decode([GitBranch].self, forKey: .branches))
        case "ui_request":
            self = .uiRequest(ui: try c.decode(UInt64.self, forKey: .ui))
        case "error":
            self = .error(
                req: try c.decodeIfPresent(UInt32.self, forKey: .req),
                id: try c.decodeIfPresent(SessionId.self, forKey: .id),
                message: try c.decode(String.self, forKey: .message))
        case "goodbye":
            self = .goodbye(try c.decode(GoodbyeReason.self, forKey: .reason))
        case "handoff_refused":
            self = .handoffRefused
        case "handoff_begin":
            self = .handoffBegin(
                format: try c.decode(UInt32.self, forKey: .format), sessions: try c.decode(UInt32.self, forKey: .sessions))
        case "size_owner":
            self = .sizeOwner(
                id: try c.decode(SessionId.self, forKey: .id),
                mine: try c.decode(Bool.self, forKey: .mine),
                owner: try c.decodeIfPresent(String.self, forKey: .owner))
        default:
            self = .unknown(type)
        }
    }
}
