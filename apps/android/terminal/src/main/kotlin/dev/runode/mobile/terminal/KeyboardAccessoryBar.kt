package dev.runode.mobile.terminal

import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.WindowInsetsSides
import androidx.compose.foundation.layout.only
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.res.stringResource

/**
 * 终端下方的按键栏：软键盘上没有的 Esc、粘住的 Ctrl、Tab、方向键，回车（软键盘收着时也按得到）和几个常用符号，横着
 * 能拖。按了直接发给 [view]。栏自己让开软键盘和系统栏（`WindowInsets.ime` 在 [WindowInsets.safeDrawing] 里），
 * 放在终端下面就贴着软键盘的上沿，软键盘收着时贴在页面底部。
 *
 * 语音输入的麦克风键和开关软键盘的键固定在右端，不跟着别的键滚走；麦克风键按下、松开都报给
 * [TerminalViewDelegate.dictation]，录音时显示成按下去的样子。开关软键盘的键在键盘收着时是「打开键盘」，开着时是
 * 按下去的样子的「收起键盘」。
 */
@Composable
fun KeyboardAccessoryBar(view: TerminalView, modifier: Modifier = Modifier, palette: KeyBarPalette? = null) {
    val keyboardOpen = view.keyboardVisible
    KeyBar(
        palette = palette,
        modifier = modifier.windowInsetsPadding(WindowInsets.safeDrawing.only(WindowInsetsSides.Horizontal + WindowInsetsSides.Bottom)),
        trailing = {
            KeyBarKey(
                title = "mic",
                icon = KeyIcons.microphone,
                label = stringResource(R.string.terminal_key_dictation),
                selected = view.dictating,
                stateDescription = if (view.dictating) stringResource(R.string.terminal_key_recording) else null,
                modifier = Modifier.pointerInput(view) {
                    // 按下、松开分开报，按住说话和点一下开始都由视图模型判断；滑出按键再松开、被打断算取消。
                    detectTapGestures(
                        onPress = {
                            view.delegate?.dictation(view, DictationPress.Down)
                            val released = tryAwaitRelease()
                            view.delegate?.dictation(view, if (released) DictationPress.Up else DictationPress.Cancel)
                        },
                    )
                },
            )
            if (!keyboardOpen) {
                KeyBarKey(
                    title = "⌨", icon = KeyIcons.keyboard, label = stringResource(R.string.terminal_key_show_keyboard),
                ) { view.showKeyboard() }
            } else {
                KeyBarKey(
                    title = "⌨", icon = KeyIcons.keyboard, label = stringResource(R.string.terminal_key_hide_keyboard),
                    selected = true,
                ) {
                    view.hideKeyboard()
                }
            }
        },
    ) {
        KeyBarKey(title = "esc", label = "Esc") { view.press(TerminalKey.Escape) }
        KeyBarKey(
            title = "ctrl", label = stringResource(R.string.terminal_key_control), selected = view.controlLatched,
        ) { view.controlLatched = !view.controlLatched }
        KeyBarKey(title = "tab", label = "Tab") { view.press(TerminalKey.Tab) }
        KeyBarKey(title = "←", icon = KeyIcons.left, label = stringResource(R.string.terminal_key_left)) {
            view.press(TerminalKey.Left)
        }
        KeyBarKey(title = "↓", icon = KeyIcons.down, label = stringResource(R.string.terminal_key_down)) {
            view.press(TerminalKey.Down)
        }
        KeyBarKey(title = "↑", icon = KeyIcons.up, label = stringResource(R.string.terminal_key_up)) {
            view.press(TerminalKey.Up)
        }
        KeyBarKey(title = "→", icon = KeyIcons.right, label = stringResource(R.string.terminal_key_right)) {
            view.press(TerminalKey.Right)
        }
        KeyBarKey(title = "⏎", icon = KeyIcons.enter, label = stringResource(R.string.terminal_key_return)) {
            view.press(TerminalKey.Enter)
        }
        for (symbol in listOf("|", "~", "/", "-", "_", "`")) {
            KeyBarKey(title = symbol, label = symbol) { view.typeText(symbol) }
        }
        KeyBarKey(title = "paste", icon = KeyIcons.paste, label = stringResource(R.string.terminal_key_paste)) { view.paste() }
    }
}
