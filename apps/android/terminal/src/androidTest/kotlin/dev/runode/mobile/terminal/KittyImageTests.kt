package dev.runode.mobile.terminal

import android.graphics.Color
import android.util.Base64
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.runode.mobile.protocol.GridSize
import dev.runode.mobile.protocol.TermSettings
import java.io.File
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlin.time.Duration.Companion.milliseconds
import org.junit.Test
import org.junit.runner.RunWith

/** 程序用 Kitty 图形协议放的图片进帧、按 id 和版本读得出像素，经真的 libghostty-vt。 */
@RunWith(AndroidJUnit4::class)
class KittyImageTests {
    private val size = GridSize(cols = 20u, rows = 5u, cellWidthPx = 8u, cellHeightPx = 16u)

    /** 2×2 像素的 RGBA：红、绿、蓝、白。 */
    private val pixels = byteArrayOf(-1, 0, 0, -1, 0, -1, 0, -1, 0, 0, -1, -1, -1, -1, -1, -1)

    /** 喂完 [bytes] 后的一份 VT 和它的一帧，交给 [check] 看，看完关掉。 */
    private fun afterFeeding(bytes: String, check: (VTerminal, ScreenFrame) -> Unit) = onMain {
        VTerminal(size, TermSettings.default).use { vt ->
            vt.feed(bytes.encodeToByteArray())
            val frame = ScreenFrame()
            vt.refresh(frame)
            check(vt, frame)
        }
    }

    @Test
    fun aPlacedImageIsInTheFrame() = afterFeeding(
        "ab\u001b_Ga=T,f=32,i=1,s=2,v=2,c=2,r=1,q=2;/wAA/wD/AP8AAP///////w==\u001b\\",
    ) { vt, frame ->
        assertEquals(1, frame.images.size)
        val image = frame.images.first()
        assertEquals(listOf(2.0, 0.0, 2.0, 1.0), listOf(image.x, image.y, image.width, image.height))
        assertEquals(Triple(2u, 2u, ImageLayer.AboveText), Triple(image.sourceWidth, image.sourceHeight, image.layer))
        assertContentEquals(pixels, vt.imagePixels(image.id, image.generation)?.rgba)
        assertNull(vt.imagePixels(image.id, image.generation + 1u))
        val bitmap = assertNotNull(vt.image(image.id, image.generation))
        assertEquals(2, bitmap.width)
        assertEquals(Color.RED, bitmap.getPixel(0, 0))
        assertEquals(Color.WHITE, bitmap.getPixel(1, 1))
    }

    /** PNG 由系统的解码器解，透明度不预乘。 */
    @Test
    fun pngImagesAreDecoded() {
        // 1×1、红色、alpha 128 的 PNG。
        val png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DQAAAEgQGALFXOsAAAAABJRU5ErkJggg=="
        afterFeeding("\u001b_Ga=T,f=100,i=1,q=2;$png\u001b\\") { vt, frame ->
            val image = assertNotNull(frame.images.firstOrNull())
            val decoded = assertNotNull(vt.imagePixels(image.id, image.generation))
            assertContentEquals(byteArrayOf(-1, 0, 0, -128), decoded.rgba)
        }
    }

    /** 从文件读的图片宿主不收，这份也不收，两边才对得上。 */
    @Test
    fun imagesFromFilesAreRefused() {
        val file = File.createTempFile("runode-kitty-", ".rgba")
        try {
            file.writeBytes(pixels)
            val path = Base64.encodeToString(file.path.encodeToByteArray(), Base64.NO_WRAP)
            afterFeeding("\u001b_Ga=T,t=f,f=32,i=1,s=2,v=2,q=2;$path\u001b\\") { _, frame ->
                assertTrue(frame.images.isEmpty())
            }
        } finally {
            file.delete()
        }
    }

    @Test
    fun aPlayingAnimationSchedulesTheNextFrame() = afterFeeding(
        "\u001b_Ga=T,f=32,i=1,s=1,v=1,q=2;/wAA/w==\u001b\\" +
            "\u001b_Ga=f,f=32,i=1,s=1,v=1,z=60,q=2;AAD//w==\u001b\\" +
            "\u001b_Ga=a,i=1,r=1,z=25,q=2\u001b\\\u001b_Ga=a,i=1,s=3,q=2\u001b\\",
    ) { vt, frame ->
        val next = assertNotNull(frame.nextImageFrame)
        assertTrue(next <= 25.milliseconds)
        vt.feed("\u001b_Ga=a,i=1,s=1,q=2\u001b\\".encodeToByteArray())
        vt.refresh(frame)
        assertNull(frame.nextImageFrame)
    }
}
