import Foundation

// 宿主那边 `runode_shared_types` 里各端共用的数据，JSON 的样子照 serde 的默认写法：
// 结构体是对象，元组结构体（`Rgb`）是数组，`rename_all = "snake_case"` 的枚举是字符串，
// 带数据的枚举（`TerminalColor::Rgb`、`IntegrationMode::Force`）是 `{"变体": 值}`。
// 读的时候比宿主宽松：缺了的字段按默认值读，不认识的取值不让整条消息失败，宿主加了字段或取值时
// 旧的 app 照样能用。

/// 一个终端会话的标识：32 个小写十六进制数字，宿主重启、交接后不变。
public struct SessionId: Hashable, Sendable, Codable, CustomStringConvertible {
    public let rawValue: String

    /// 写法不对（不是正好 32 个十六进制数字）时返回 `nil`。
    public init?(_ text: String) {
        guard text.count == 32, text.allSatisfy(\.isHexDigit) else { return nil }
        rawValue = text.lowercased()
    }

    public var description: String { rawValue }

    public init(from decoder: any Decoder) throws {
        let text = try decoder.singleValueContainer().decode(String.self)
        guard let id = SessionId(text) else {
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "a session id is 32 hexadecimal digits"))
        }
        self = id
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }
}

/// 网格的行列数和单元格的像素尺寸。
public struct GridSize: Hashable, Sendable, Codable {
    public var cols: UInt16
    public var rows: UInt16
    public var cellWidthPx: UInt16
    public var cellHeightPx: UInt16

    public init(cols: UInt16, rows: UInt16, cellWidthPx: UInt16, cellHeightPx: UInt16) {
        self.cols = cols
        self.rows = rows
        self.cellWidthPx = cellWidthPx
        self.cellHeightPx = cellHeightPx
    }

    enum CodingKeys: String, CodingKey {
        case cols, rows
        case cellWidthPx = "cell_width_px"
        case cellHeightPx = "cell_height_px"
    }
}

/// 一个 RGB 颜色，JSON 里是 `[r, g, b]`。
public struct Rgb: Hashable, Sendable, Codable {
    public var r: UInt8
    public var g: UInt8
    public var b: UInt8

    public init(_ r: UInt8, _ g: UInt8, _ b: UInt8) {
        self.r = r
        self.g = g
        self.b = b
    }

    /// `0xRRGGBB`。
    public init(hex: UInt32) {
        self.init(UInt8((hex >> 16) & 0xFF), UInt8((hex >> 8) & 0xFF), UInt8(hex & 0xFF))
    }

    public init(from decoder: any Decoder) throws {
        var container = try decoder.unkeyedContainer()
        self.init(
            try container.decode(UInt8.self), try container.decode(UInt8.self), try container.decode(UInt8.self))
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.unkeyedContainer()
        try container.encode(r)
        try container.encode(g)
        try container.encode(b)
    }
}

/// 光标和选区的颜色：固定色，或者跟随所在单元格的前景、背景色。
public enum TerminalColor: Hashable, Sendable, Codable {
    case rgb(Rgb)
    case cellForeground
    case cellBackground

    private enum Key: String, CodingKey { case rgb }

    public init(from decoder: any Decoder) throws {
        if let text = try? decoder.singleValueContainer().decode(String.self) {
            switch text {
            case "cell_foreground": self = .cellForeground
            case "cell_background": self = .cellBackground
            default:
                throw DecodingError.dataCorrupted(
                    .init(codingPath: decoder.codingPath, debugDescription: "unknown terminal color \(text)"))
            }
            return
        }
        let container = try decoder.container(keyedBy: Key.self)
        self = .rgb(try container.decode(Rgb.self, forKey: .rgb))
    }

    public func encode(to encoder: any Encoder) throws {
        switch self {
        case .rgb(let color):
            var container = encoder.container(keyedBy: Key.self)
            try container.encode(color, forKey: .rgb)
        case .cellForeground:
            var container = encoder.singleValueContainer()
            try container.encode("cell_foreground")
        case .cellBackground:
            var container = encoder.singleValueContainer()
            try container.encode("cell_background")
        }
    }
}

/// 配置的光标样式。
public enum CursorStyle: String, Hashable, Sendable, Codable {
    case block
    case blockHollow = "block_hollow"
    case bar
    case underline
}

/// macOS 上 Option 键当不当 Alt 用；手机的硬件键盘上 Option 键照同样的设置处理。
public enum OptionAsAlt: String, Hashable, Sendable, Codable {
    case no = "false"
    case yes = "true"
    case left
    case right
}

/// 宿主 VT 套着的主题，`HostMsg.attached`、`HostMsg.themeApplied` 里带来。和宿主的 `TermSettings`
/// 字段一一对应；编码时每个字段都写（宿主那边这些字段都是必填的）。
public struct TermSettings: Hashable, Sendable, Codable {
    public var background: Rgb
    public var foreground: Rgb
    /// 覆盖默认 256 色中的若干项：`(下标, 颜色)`。
    public var palette: [PaletteEntry]
    public var cursorStyle: CursorStyle
    /// `nil` 表示默认闪烁。
    public var cursorBlink: Bool?
    /// `nil` 表示用前景色。
    public var cursorColor: TerminalColor?
    /// 实心块状光标下文字的颜色，`nil` 表示用背景色。
    public var cursorText: TerminalColor?
    public var selectionBackground: TerminalColor?
    public var selectionForeground: TerminalColor?
    public var searchBackground: TerminalColor
    public var searchForeground: TerminalColor
    public var searchSelectedBackground: TerminalColor
    public var searchSelectedForeground: TerminalColor
    public var optionAsAlt: OptionAsAlt
    /// 回滚历史最多占多少字节。
    public var scrollbackLimit: UInt64

    /// 调色板里的一项，JSON 里是 `[下标, [r, g, b]]`。
    public struct PaletteEntry: Hashable, Sendable, Codable {
        public var index: UInt8
        public var color: Rgb

        public init(index: UInt8, color: Rgb) {
            self.index = index
            self.color = color
        }

        public init(from decoder: any Decoder) throws {
            var container = try decoder.unkeyedContainer()
            index = try container.decode(UInt8.self)
            color = try container.decode(Rgb.self)
        }

        public func encode(to encoder: any Encoder) throws {
            var container = encoder.unkeyedContainer()
            try container.encode(index)
            try container.encode(color)
        }
    }

    /// 宿主没配置时的设置，和 `TermSettings::default()`（`runode_shared_types::theme` 的默认配色）一致。
    public static let `default` = TermSettings(
        background: Rgb(hex: 0x171618),
        foreground: Rgb(hex: 0xE6E1D8),
        palette: [
            0x393A3D, 0xFF1261, 0x2AD947, 0xFCBA28, 0x2D9AFF, 0xDD30FF, 0x17D5DF, 0xE7E7E7,
            0x6B6B6B, 0xC55555, 0xAAC474, 0xFECA88, 0x82B8C8, 0xC28CB8, 0x93D3C3, 0xF8F8F8,
        ].enumerated().map { PaletteEntry(index: UInt8($0.offset), color: Rgb(hex: UInt32($0.element))) },
        cursorStyle: .block,
        cursorBlink: nil,
        cursorColor: nil,
        cursorText: nil,
        selectionBackground: nil,
        selectionForeground: nil,
        searchBackground: .rgb(Rgb(hex: 0xFFE795)),
        searchForeground: .rgb(Rgb(hex: 0x000000)),
        searchSelectedBackground: .rgb(Rgb(hex: 0xF2A57E)),
        searchSelectedForeground: .rgb(Rgb(hex: 0x000000)),
        optionAsAlt: .no,
        scrollbackLimit: 10 * 1024 * 1024
    )

    public init(
        background: Rgb, foreground: Rgb, palette: [PaletteEntry], cursorStyle: CursorStyle, cursorBlink: Bool?,
        cursorColor: TerminalColor?, cursorText: TerminalColor?, selectionBackground: TerminalColor?,
        selectionForeground: TerminalColor?, searchBackground: TerminalColor, searchForeground: TerminalColor,
        searchSelectedBackground: TerminalColor, searchSelectedForeground: TerminalColor, optionAsAlt: OptionAsAlt,
        scrollbackLimit: UInt64
    ) {
        self.background = background
        self.foreground = foreground
        self.palette = palette
        self.cursorStyle = cursorStyle
        self.cursorBlink = cursorBlink
        self.cursorColor = cursorColor
        self.cursorText = cursorText
        self.selectionBackground = selectionBackground
        self.selectionForeground = selectionForeground
        self.searchBackground = searchBackground
        self.searchForeground = searchForeground
        self.searchSelectedBackground = searchSelectedBackground
        self.searchSelectedForeground = searchSelectedForeground
        self.optionAsAlt = optionAsAlt
        self.scrollbackLimit = scrollbackLimit
    }

    enum CodingKeys: String, CodingKey {
        case background, foreground, palette
        case cursorStyle = "cursor_style"
        case cursorBlink = "cursor_blink"
        case cursorColor = "cursor_color"
        case cursorText = "cursor_text"
        case selectionBackground = "selection_background"
        case selectionForeground = "selection_foreground"
        case searchBackground = "search_background"
        case searchForeground = "search_foreground"
        case searchSelectedBackground = "search_selected_background"
        case searchSelectedForeground = "search_selected_foreground"
        case optionAsAlt = "option_as_alt"
        case scrollbackLimit = "scrollback_limit"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let fallback = TermSettings.default
        background = try c.decodeIfPresent(Rgb.self, forKey: .background) ?? fallback.background
        foreground = try c.decodeIfPresent(Rgb.self, forKey: .foreground) ?? fallback.foreground
        palette = try c.decodeIfPresent([PaletteEntry].self, forKey: .palette) ?? fallback.palette
        cursorStyle = (try? c.decodeIfPresent(CursorStyle.self, forKey: .cursorStyle)) ?? fallback.cursorStyle
        cursorBlink = try c.decodeIfPresent(Bool.self, forKey: .cursorBlink)
        cursorColor = try? c.decodeIfPresent(TerminalColor.self, forKey: .cursorColor)
        cursorText = try? c.decodeIfPresent(TerminalColor.self, forKey: .cursorText)
        selectionBackground = try? c.decodeIfPresent(TerminalColor.self, forKey: .selectionBackground)
        selectionForeground = try? c.decodeIfPresent(TerminalColor.self, forKey: .selectionForeground)
        searchBackground =
            (try? c.decodeIfPresent(TerminalColor.self, forKey: .searchBackground)) ?? fallback.searchBackground
        searchForeground =
            (try? c.decodeIfPresent(TerminalColor.self, forKey: .searchForeground)) ?? fallback.searchForeground
        searchSelectedBackground =
            (try? c.decodeIfPresent(TerminalColor.self, forKey: .searchSelectedBackground))
            ?? fallback.searchSelectedBackground
        searchSelectedForeground =
            (try? c.decodeIfPresent(TerminalColor.self, forKey: .searchSelectedForeground))
            ?? fallback.searchSelectedForeground
        optionAsAlt = (try? c.decodeIfPresent(OptionAsAlt.self, forKey: .optionAsAlt)) ?? fallback.optionAsAlt
        scrollbackLimit = try c.decodeIfPresent(UInt64.self, forKey: .scrollbackLimit) ?? fallback.scrollbackLimit
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(background, forKey: .background)
        try c.encode(foreground, forKey: .foreground)
        try c.encode(palette, forKey: .palette)
        try c.encode(cursorStyle, forKey: .cursorStyle)
        try c.encode(cursorBlink, forKey: .cursorBlink)
        try c.encode(cursorColor, forKey: .cursorColor)
        try c.encode(cursorText, forKey: .cursorText)
        try c.encode(selectionBackground, forKey: .selectionBackground)
        try c.encode(selectionForeground, forKey: .selectionForeground)
        try c.encode(searchBackground, forKey: .searchBackground)
        try c.encode(searchForeground, forKey: .searchForeground)
        try c.encode(searchSelectedBackground, forKey: .searchSelectedBackground)
        try c.encode(searchSelectedForeground, forKey: .searchSelectedForeground)
        try c.encode(optionAsAlt, forKey: .optionAsAlt)
        try c.encode(scrollbackLimit, forKey: .scrollbackLimit)
    }
}

/// agent 当前的状态。宿主新加的状态读成 `unknown`，界面当作没有 agent 状态处理。
public enum AgentState: Hashable, Sendable, Codable {
    case working
    case idle
    /// 停下来等用户回答。
    case blocked
    case unknown(String)

    public init(from decoder: any Decoder) throws {
        let text = try decoder.singleValueContainer().decode(String.self)
        switch text {
        case "working": self = .working
        case "idle": self = .idle
        case "blocked": self = .blocked
        default: self = .unknown(text)
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .working: try container.encode("working")
        case .idle: try container.encode("idle")
        case .blocked: try container.encode("blocked")
        case .unknown(let text): try container.encode(text)
        }
    }
}

/// 是哪个 agent：宿主 `AgentKind` 的短名（`claude`、`codex` 等）。按字符串存，宿主新认得的 agent
/// 不让消息解析失败。
public struct AgentKind: Hashable, Sendable, Codable {
    public let label: String

    public init(_ label: String) {
        self.label = label
    }

    public init(from decoder: any Decoder) throws {
        label = try decoder.singleValueContainer().decode(String.self)
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(label)
    }

    /// 给人看的名字，和宿主的 `AgentKind::display_name` 一致；不认识的原样显示。
    public var displayName: String {
        let names: [String: String] = [
            "pi": "Pi", "claude": "Claude Code", "codex": "Codex", "gemini": "Gemini CLI", "cursor": "Cursor Agent",
            "devin": "Devin", "antigravity": "Antigravity", "cline": "Cline", "omp": "Oh My Pi",
            "mastracode": "Mastra Code", "open_code": "OpenCode", "github_copilot": "GitHub Copilot",
            "kimi": "Kimi Code", "kiro": "Kiro", "droid": "Droid", "amp": "Amp", "grok": "Grok", "hermes": "Hermes",
            "kilo": "Kilo Code", "qodercli": "Qoder CLI", "qwen": "Qwen Code", "letta": "Letta Code", "maki": "Maki",
            "muse": "Muse",
            "aider": "Aider", "goose": "Goose", "crush": "Crush", "auggie": "Auggie",
            "continue_cli": "Continue", "junie": "Junie", "open_hands": "OpenHands", "trae": "Trae Agent",
            "code_buddy": "CodeBuddy Code", "iflow": "iFlow CLI", "codebuff": "Codebuff", "mistral_vibe": "Mistral Vibe",
            "jules": "Jules", "plandex": "Plandex",
            "other": "Agent",
        ]
        return names[label] ?? label
    }
}

/// 前台 agent 和它的状态。
public struct Agent: Hashable, Sendable, Codable {
    public var kind: AgentKind
    public var state: AgentState

    public init(kind: AgentKind, state: AgentState) {
        self.kind = kind
        self.state = state
    }
}

/// 一个终端会话对外公布的状态。只读界面用得到的字段，其余（shell 报告的名字、PATH 等）忽略；
/// 缺的字段按默认值读。
public struct SessionMeta: Hashable, Sendable, Decodable {
    public var title: String?
    public var fallbackTitle: String?
    public var agent: Agent?
    public var cwd: String?
    public var foregroundIsShell: Bool
    public var foreground: String?

    public init(
        title: String? = nil, fallbackTitle: String? = nil, agent: Agent? = nil, cwd: String? = nil,
        foregroundIsShell: Bool = false, foreground: String? = nil
    ) {
        self.title = title
        self.fallbackTitle = fallbackTitle
        self.agent = agent
        self.cwd = cwd
        self.foregroundIsShell = foregroundIsShell
        self.foreground = foreground
    }

    enum CodingKeys: String, CodingKey {
        case title, agent, cwd, foreground
        case fallbackTitle = "fallback_title"
        case foregroundIsShell = "foreground_is_shell"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        title = try c.decodeIfPresent(String.self, forKey: .title)
        fallbackTitle = try c.decodeIfPresent(String.self, forKey: .fallbackTitle)
        agent = try? c.decodeIfPresent(Agent.self, forKey: .agent)
        // 宿主的 `PathBuf` 不是合法 UTF-8 时序列化不出来，能读到的都是字符串。
        cwd = try? c.decodeIfPresent(String.self, forKey: .cwd)
        foregroundIsShell = try c.decodeIfPresent(Bool.self, forKey: .foregroundIsShell) ?? false
        foreground = try c.decodeIfPresent(String.self, forKey: .foreground)
    }

    /// 标签上显示的名字：程序设置的标题，没有时用前台程序名或目录名。
    public var displayTitle: String? {
        if let title, !title.isEmpty { return title }
        if let fallbackTitle, !fallbackTitle.isEmpty { return fallbackTitle }
        return nil
    }
}

/// shell 的种类。
public enum Shell: String, Hashable, Sendable, Codable {
    case zsh, bash, fish
}

/// shell 集成怎么注入，JSON 里是 `"detect"`、`"off"` 或 `{"force": "zsh"}`。
public enum IntegrationMode: Hashable, Sendable, Codable {
    case detect
    case off
    case force(Shell)

    private enum Key: String, CodingKey { case force }

    public init(from decoder: any Decoder) throws {
        if let text = try? decoder.singleValueContainer().decode(String.self) {
            switch text {
            case "detect": self = .detect
            case "off": self = .off
            default:
                throw DecodingError.dataCorrupted(
                    .init(codingPath: decoder.codingPath, debugDescription: "unknown integration mode \(text)"))
            }
            return
        }
        let container = try decoder.container(keyedBy: Key.self)
        self = .force(try container.decode(Shell.self, forKey: .force))
    }

    public func encode(to encoder: any Encoder) throws {
        switch self {
        case .detect:
            var container = encoder.singleValueContainer()
            try container.encode("detect")
        case .off:
            var container = encoder.singleValueContainer()
            try container.encode("off")
        case .force(let shell):
            var container = encoder.container(keyedBy: Key.self)
            try container.encode(shell, forKey: .force)
        }
    }
}
