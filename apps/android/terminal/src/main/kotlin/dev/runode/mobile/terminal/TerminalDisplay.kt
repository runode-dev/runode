package dev.runode.mobile.terminal

import dev.runode.mobile.protocol.TermSettings

/** 用户在终端视图里打的东西，由持有 VT 的一方按 VT 当前的模式编码后发给宿主。 */
sealed class TerminalInput {
    /** 一次按键（硬件键盘、辅助栏、粘住 Ctrl 时打的字）。 */
    data class Key(val input: KeyInput) : TerminalInput()

    /** 软键盘、输入法上屏的文字，原样发送。 */
    data class Text(val text: String) : TerminalInput()

    /** 粘贴的文字，按括号粘贴的规矩编码。 */
    data class Paste(val text: String) : TerminalInput()

    /**
     * 程序自己管滚动时（[VTerminal.programScrolls]）在第 [row] 行第 [column] 列滚了 [lines] 行，负数往上看更早的
     * 内容。
     */
    data class Wheel(val lines: Int, val column: Int, val row: Int) : TerminalInput()

    /** 程序开着鼠标上报时在第 [row] 行第 [column] 列点了一下（全屏 agent 界面里的按钮、链接）。 */
    data class Click(val column: Int, val row: Int) : TerminalInput()
}

/** 辅助栏上麦克风键的动作。 */
enum class DictationPress {
    Down,
    Up,

    /** 手指滑出按键再松开，或者被系统打断。 */
    Cancel,
}

/** 画终端的一方。视图模型在 VT 换了、内容变了、响铃时通知它；它自己从 VT 读要画的东西。都在主线程上调。 */
interface TerminalDisplay {
    /** 换了一份 VT（重新 `Attach` 时新建的），或者没有了（`null`）。 */
    fun terminalDidReset(terminal: VTerminal?, settings: TermSettings)

    /** VT 的内容变了（喂了输出、改了尺寸）；画的一方自己决定什么时候刷新。 */
    fun terminalContentDidChange()

    fun terminalSettingsDidChange(settings: TermSettings)

    fun terminalDidRingBell()

    /**
     * 尺寸方式换了：适配手机时网格马上会变成正好铺满视图的大小，视图回到不缩放；跟随电脑时按可读的最小字号缩放，
     * 超出的部分横向平移。
     */
    fun terminalSizeModeDidChange(fitsPhone: Boolean)

    /** 用户要打字：唤起键盘。 */
    fun terminalShowKeyboard()
}
