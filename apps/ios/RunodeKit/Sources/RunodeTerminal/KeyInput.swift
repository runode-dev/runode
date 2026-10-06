internal import GhosttyVt

/// 修饰键。
public struct KeyModifiers: OptionSet, Sendable, Hashable {
    public let rawValue: UInt8
    public init(rawValue: UInt8) { self.rawValue = rawValue }

    public static let shift = KeyModifiers(rawValue: 1 << 0)
    public static let control = KeyModifiers(rawValue: 1 << 1)
    public static let alt = KeyModifiers(rawValue: 1 << 2)
    public static let command = KeyModifiers(rawValue: 1 << 3)

    /// libghostty 的 `GhosttyMods` 位：shift 1、ctrl 2、alt 4、super 8，alt 在右边时另置一位。
    func ghosttyMods(rightAlt: Bool) -> GhosttyMods {
        var mods: GhosttyMods = 0
        if contains(.shift) { mods |= 1 << 0 }
        if contains(.control) { mods |= 1 << 1 }
        if contains(.alt) {
            mods |= 1 << 2
            if rightAlt { mods |= 1 << 8 }
        }
        if contains(.command) { mods |= 1 << 3 }
        return mods
    }
}

/// 按下的是哪个键，按美式键盘的物理位置算（和 W3C 的 `code` 一样）。
public enum TerminalKey: Sendable, Hashable {
    /// 字母、数字和符号键；带的字符是这个键不按 Shift 时的字符（`a`、`1`、`-` 等）。
    case character(Character)
    case space, enter, tab, backspace, escape
    case insert, delete, home, end, pageUp, pageDown
    case up, down, left, right
    /// F1 到 F24。
    case function(Int)

    /// 美式键盘上打出 `character` 要按哪个键、要不要 Shift。认不得的字符（中文等）返回 `nil`。
    public static func forTyped(_ character: Character) -> (key: TerminalKey, shift: Bool)? {
        if character == " " { return (.space, false) }
        guard let scalar = character.unicodeScalars.first, character.unicodeScalars.count == 1, scalar.isASCII else {
            return nil
        }
        if ("a"..."z").contains(character) { return (.character(character), false) }
        if ("A"..."Z").contains(character) { return (.character(Character(character.lowercased())), true) }
        if ("0"..."9").contains(character) { return (.character(character), false) }
        let unshifted: [Character: Character] = [
            "!": "1", "@": "2", "#": "3", "$": "4", "%": "5", "^": "6", "&": "7", "*": "8", "(": "9", ")": "0",
            "_": "-", "+": "=", "{": "[", "}": "]", "|": "\\", ":": ";", "\"": "'", "<": ",", ">": ".", "?": "/",
            "~": "`",
        ]
        if let base = unshifted[character] { return (.character(base), true) }
        if "-=[]\\;',./`".contains(character) { return (.character(character), false) }
        return nil
    }

    var ghosttyKey: GhosttyKey {
        switch self {
        case .space: return GHOSTTY_KEY_SPACE
        case .enter: return GHOSTTY_KEY_ENTER
        case .tab: return GHOSTTY_KEY_TAB
        case .backspace: return GHOSTTY_KEY_BACKSPACE
        case .escape: return GHOSTTY_KEY_ESCAPE
        case .insert: return GHOSTTY_KEY_INSERT
        case .delete: return GHOSTTY_KEY_DELETE
        case .home: return GHOSTTY_KEY_HOME
        case .end: return GHOSTTY_KEY_END
        case .pageUp: return GHOSTTY_KEY_PAGE_UP
        case .pageDown: return GHOSTTY_KEY_PAGE_DOWN
        case .up: return GHOSTTY_KEY_ARROW_UP
        case .down: return GHOSTTY_KEY_ARROW_DOWN
        case .left: return GHOSTTY_KEY_ARROW_LEFT
        case .right: return GHOSTTY_KEY_ARROW_RIGHT
        case .function(let n):
            guard (1...24).contains(n) else { return GHOSTTY_KEY_UNIDENTIFIED }
            return GhosttyKey(rawValue: GHOSTTY_KEY_F1.rawValue + Int32(n - 1))
        case .character(let character):
            guard let scalar = character.unicodeScalars.first?.value else { return GHOSTTY_KEY_UNIDENTIFIED }
            switch character {
            case "a"..."z": return GhosttyKey(rawValue: GHOSTTY_KEY_A.rawValue + Int32(scalar - 0x61))
            case "0"..."9": return GhosttyKey(rawValue: GHOSTTY_KEY_DIGIT_0.rawValue + Int32(scalar - 0x30))
            case "`": return GHOSTTY_KEY_BACKQUOTE
            case "\\": return GHOSTTY_KEY_BACKSLASH
            case "[": return GHOSTTY_KEY_BRACKET_LEFT
            case "]": return GHOSTTY_KEY_BRACKET_RIGHT
            case ",": return GHOSTTY_KEY_COMMA
            case "=": return GHOSTTY_KEY_EQUAL
            case "-": return GHOSTTY_KEY_MINUS
            case ".": return GHOSTTY_KEY_PERIOD
            case "'": return GHOSTTY_KEY_QUOTE
            case ";": return GHOSTTY_KEY_SEMICOLON
            case "/": return GHOSTTY_KEY_SLASH
            default: return GHOSTTY_KEY_UNIDENTIFIED
            }
        }
    }
}

/// 一次按键：哪个键、按着哪些修饰键、打出的文字。
public struct KeyInput: Sendable, Hashable {
    public var key: TerminalKey
    public var modifiers: KeyModifiers
    /// 平台按当前键盘布局和修饰键打出的文字；没有时为空。
    public var text: String?
    /// 不按修饰键时这个键打出的字符。
    public var unshifted: Character?
    /// 平台已经用掉、体现在 `text` 里的修饰键（比如打出 `A` 的 Shift）。
    public var consumedModifiers: KeyModifiers
    /// 按的是右边的 Alt/Option。
    public var rightAlt: Bool

    public init(
        key: TerminalKey, modifiers: KeyModifiers = [], text: String? = nil, unshifted: Character? = nil,
        consumedModifiers: KeyModifiers = [], rightAlt: Bool = false
    ) {
        self.key = key
        self.modifiers = modifiers
        self.text = text
        self.unshifted = unshifted
        self.consumedModifiers = consumedModifiers
        self.rightAlt = rightAlt
    }

    /// 打一个字符，`modifiers` 是另外按着的（比如辅助栏上粘住的 Ctrl）。美式键盘上找不到这个字符
    /// 时返回 `nil`。
    public static func typing(_ character: Character, modifiers: KeyModifiers = []) -> KeyInput? {
        guard let (key, shift) = TerminalKey.forTyped(character) else { return nil }
        var unshifted: Character?
        if case .character(let base) = key { unshifted = base } else if key == .space { unshifted = " " }
        var all = modifiers
        if shift { all.insert(.shift) }
        return KeyInput(
            key: key, modifiers: all, text: String(character), unshifted: unshifted,
            consumedModifiers: shift ? .shift : [])
    }
}
