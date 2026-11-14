package dev.runode.mobile.terminal

import android.graphics.Paint
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/** 要系统字体和 ICU 的部分：打包的符号字体、默认按文字画的 emoji。 */
@RunWith(AndroidJUnit4::class)
class SymbolFontTests {
    /**
     * 打包的 Symbols Nerd Font Mono 能加载，提示符里常见的私用区图标都有字形：Powerline 的分支符号、Font Awesome 的
     * Apple 标志、辅助平面上的 Material Design 图标。
     */
    @Test
    fun bundledSymbolFontHasPromptIcons() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val typeface = assertNotNull(SymbolFont.typeface(context))
        val paint = Paint().apply { this.typeface = typeface }
        for (codepoint in listOf(0xE0A0, 0xE0B0, 0xF113, 0xF0001)) {
            assertTrue(paint.hasGlyph(String(Character.toChars(codepoint))), "U+${codepoint.toString(16)}")
        }
    }

    /** 默认按文字画的 `⏺`、`✳` 补上文字样式选择符；默认就是 emoji 的 `❗`、ASCII、已经带了选择符的不动。 */
    @Test
    fun textPresentationMarksTextDefaultEmoji() {
        assertEquals("⏺︎ 改好了", TextPresentation.apply("⏺ 改好了"))
        assertEquals("✳︎", TextPresentation.apply("✳"))
        assertEquals("❗ #1 *", TextPresentation.apply("❗ #1 *"))
        assertEquals("✔️", TextPresentation.apply("✔️"))
    }
}
