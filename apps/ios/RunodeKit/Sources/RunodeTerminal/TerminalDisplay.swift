import RunodeProtocol

/// 用户在终端视图里打的东西，由持有 VT 的一方按 VT 当前的模式编码后发给宿主。
public enum TerminalInput: Sendable, Hashable {
    /// 一次按键（硬件键盘、辅助栏、粘住 Ctrl 时打的字）。
    case key(KeyInput)
    /// 软键盘、输入法上屏的文字，原样发送。
    case text(String)
    /// 粘贴的文字，按括号粘贴的规矩编码。
    case paste(String)
    /// 程序自己管滚动时（见 `VTerminal.programScrolls`）在第 `row` 行第 `column` 列滚了 `lines` 行，
    /// 负数往上看更早的内容。
    case wheel(lines: Int, column: Int, row: Int)
    /// 程序开着鼠标上报时在第 `row` 行第 `column` 列点了一下（全屏 agent 界面里的按钮、链接）。
    case click(column: Int, row: Int)
}

/// 画终端的一方。视图模型在 VT 换了、内容变了、响铃时通知它；它自己从 VT 读要画的东西。
/// 现在是 CoreText 画的 `TerminalView`，以后换 Metal 也只要实现这个协议。
@MainActor
public protocol TerminalDisplay: AnyObject {
    /// 换了一份 VT（重新 `Attach` 时新建的），或者没有了（`nil`）。
    func terminalDidReset(_ terminal: VTerminal?, settings: TermSettings)
    /// VT 的内容变了（喂了输出、改了尺寸）；画的一方自己决定什么时候刷新。
    func terminalContentDidChange()
    /// 换了主题。
    func terminalSettingsDidChange(_ settings: TermSettings)
    /// 程序响了铃。
    func terminalDidRingBell()
    /// 尺寸方式换了：适配手机时网格马上会变成正好铺满视图的大小，视图回到不缩放；跟随电脑时按可读的
    /// 最小字号缩放，超出的部分横向平移。
    func terminalSizeModeDidChange(fitsPhone: Bool)
    /// 用户要打字：唤起键盘。
    func terminalShowKeyboard()
}
