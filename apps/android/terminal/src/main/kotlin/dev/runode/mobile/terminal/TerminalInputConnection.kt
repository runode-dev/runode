package dev.runode.mobile.terminal

import android.text.Editable
import android.view.inputmethod.BaseInputConnection

/**
 * 软键盘、输入法和终端之间的连接。终端没有可以编辑的文档：已经发出去的字归程序管，这里的文档
 * （[TerminalView.document]）平常是空的，只在输入法组字时装着组字中的文字。上屏（[commitText]、
 * [finishComposingText]）后文档里组字以外的字整段发给程序、从文档里拿掉；退格在组字时归输入法，不组字时发退格键。 */
internal class TerminalInputConnection(private val view: TerminalView) : BaseInputConnection(view, true) {
    override fun getEditable(): Editable = view.document

    override fun commitText(text: CharSequence?, newCursorPosition: Int): Boolean {
        super.commitText(text, newCursorPosition)
        view.flushDocument()
        return true
    }

    override fun setComposingText(text: CharSequence?, newCursorPosition: Int): Boolean {
        super.setComposingText(text, newCursorPosition)
        view.flushDocument()
        return true
    }

    override fun setComposingRegion(start: Int, end: Int): Boolean {
        super.setComposingRegion(start, end)
        view.markedTextDidChange()
        return true
    }

    override fun finishComposingText(): Boolean {
        super.finishComposingText()
        view.flushDocument()
        return true
    }

    /**
     * 不组字时文档是空的，删几个字就发几个退格（往后删发 Delete）。组字时按 [BaseInputConnection] 的规矩只删组字文字
     * 以外的，文档里那部分是空的，什么也不删、不发：发退格会删掉已经上屏的字，组字文字由输入法经 [setComposingText] 改。
     */
    override fun deleteSurroundingText(beforeLength: Int, afterLength: Int): Boolean {
        if (view.document.isEmpty()) {
            repeat(beforeLength) { view.press(TerminalKey.Backspace) }
            repeat(afterLength) { view.press(TerminalKey.Delete) }
            return true
        }
        super.deleteSurroundingText(beforeLength, afterLength)
        view.markedTextDidChange()
        return true
    }

    override fun deleteSurroundingTextInCodePoints(beforeLength: Int, afterLength: Int): Boolean {
        if (view.document.isEmpty()) return deleteSurroundingText(beforeLength, afterLength)
        super.deleteSurroundingTextInCodePoints(beforeLength, afterLength)
        view.markedTextDidChange()
        return true
    }

    override fun setSelection(start: Int, end: Int): Boolean {
        super.setSelection(start, end)
        view.markedTextDidChange()
        return true
    }

    /** 软键盘上的「完成」「发送」这类动作键当回车。 */
    override fun performEditorAction(editorAction: Int): Boolean {
        view.press(TerminalKey.Enter)
        return true
    }

    override fun performContextMenuAction(id: Int): Boolean {
        if (id == android.R.id.paste || id == android.R.id.pasteAsPlainText) {
            view.paste()
            return true
        }
        return super.performContextMenuAction(id)
    }
}
