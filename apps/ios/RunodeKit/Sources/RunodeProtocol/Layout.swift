import Foundation

/// 电脑上的 app 里各个终端摆在哪：窗口、工作区、标签和标签里的分屏，回 `Layout`。和宿主的
/// `WindowLayout` 等一一对应；序号一律从 1 开始，窗口按打开的先后，工作区、标签按界面上的先后，
/// 分屏按标签里从左到右、从上到下。
public struct WindowLayout: Hashable, Sendable, Decodable {
    public var index: UInt32
    /// 是最前面的那个窗口。
    public var front: Bool
    public var workspaces: [WorkspaceLayout]

    public init(index: UInt32, front: Bool = false, workspaces: [WorkspaceLayout]) {
        self.index = index
        self.front = front
        self.workspaces = workspaces
    }

    enum CodingKeys: String, CodingKey { case index, front, workspaces }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        index = try c.decode(UInt32.self, forKey: .index)
        front = try c.decodeIfPresent(Bool.self, forKey: .front) ?? false
        workspaces = try c.decode([WorkspaceLayout].self, forKey: .workspaces)
    }
}

/// 窗口里的一个工作区。
public struct WorkspaceLayout: Hashable, Sendable, Decodable {
    public var index: UInt32
    /// 侧栏里显示的名字。
    public var name: String?
    /// 工作区的目录；旧的电脑不报。
    public var dir: String?
    /// 是窗口当前显示的工作区。
    public var active: Bool
    public var tabs: [TabLayout]

    public init(index: UInt32, name: String? = nil, dir: String? = nil, active: Bool = false, tabs: [TabLayout]) {
        self.index = index
        self.name = name
        self.dir = dir
        self.active = active
        self.tabs = tabs
    }

    enum CodingKeys: String, CodingKey { case index, name, dir, active, tabs }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        index = try c.decode(UInt32.self, forKey: .index)
        name = try c.decodeIfPresent(String.self, forKey: .name)
        dir = try c.decodeIfPresent(String.self, forKey: .dir)
        active = try c.decodeIfPresent(Bool.self, forKey: .active) ?? false
        tabs = try c.decode([TabLayout].self, forKey: .tabs)
    }
}

/// 工作区里的一个标签。
public struct TabLayout: Hashable, Sendable, Decodable {
    public var index: UInt32
    /// 是工作区当前显示的标签。
    public var active: Bool
    public var panes: [PaneLayout]

    public init(index: UInt32, active: Bool = false, panes: [PaneLayout]) {
        self.index = index
        self.active = active
        self.panes = panes
    }

    enum CodingKeys: String, CodingKey { case index, active, panes }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        index = try c.decode(UInt32.self, forKey: .index)
        active = try c.decodeIfPresent(Bool.self, forKey: .active) ?? false
        panes = try c.decode([PaneLayout].self, forKey: .panes)
    }
}

/// 标签里的一个分屏，即一个终端。
public struct PaneLayout: Hashable, Sendable, Decodable {
    public var index: UInt32
    public var id: SessionId
    /// 在标签里的位置；读不出来时为空。
    public var rect: PaneRect?
    /// 是标签里有焦点的那个分屏。
    public var focused: Bool

    public init(index: UInt32, id: SessionId, rect: PaneRect? = nil, focused: Bool = false) {
        self.index = index
        self.id = id
        self.rect = rect
        self.focused = focused
    }

    enum CodingKeys: String, CodingKey { case index, id, rect, focused }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        index = try c.decode(UInt32.self, forKey: .index)
        id = try c.decode(SessionId.self, forKey: .id)
        rect = try? c.decodeIfPresent(PaneRect.self, forKey: .rect)
        focused = try c.decodeIfPresent(Bool.self, forKey: .focused) ?? false
    }
}

/// 分屏在标签区域里的位置，整个标签区域按 0..`extent` 归一化，和电脑上窗口的像素大小无关。
public struct PaneRect: Hashable, Sendable, Decodable {
    /// 整个标签区域的边长。
    public static let extent: UInt16 = 1000

    public var x: UInt16
    public var y: UInt16
    public var width: UInt16
    public var height: UInt16

    public init(x: UInt16, y: UInt16, width: UInt16, height: UInt16) {
        self.x = x
        self.y = y
        self.width = width
        self.height = height
    }
}

extension WorkspaceLayout {
    /// 工作区里的会话：按标签的先后、标签里分屏的先后。
    public var sessions: [SessionId] {
        tabs.flatMap { $0.panes.map(\.id) }
    }

    /// 在这个工作区里开新标签时挨着的会话：当前标签里有焦点的分屏，没标出来时退到第一个分屏。
    public var anchor: SessionId? {
        let tab = tabs.first(where: \.active) ?? tabs.first
        return (tab?.panes.first(where: \.focused) ?? tab?.panes.first)?.id
    }
}
