package dev.runode.mobile.connection

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.runode.mobile.protocol.tunnelALPN
import java.io.IOException
import kotlin.test.assertFailsWith
import kotlin.test.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * librunode_tunnel.so 真的加载得上、JNI 的名字和签名对得上，异常按 [IrohNative] 说的抛。端点按 n0 的预设会去问中继和
 * DNS。
 */
@RunWith(AndroidJUnit4::class)
class IrohLibraryTests {
    private val alpn = tunnelALPN.encodeToByteArray()

    @Test
    fun bindsAndClosesAnEndpoint() {
        NativeIroh.context = InstrumentationRegistry.getInstrumentation().targetContext
        val endpoint = NativeIroh.bind()
        assertTrue(endpoint > 0)
        NativeIroh.closeEndpoint(endpoint)
        NativeIroh.closeEndpoint(endpoint)
        assertFailsWith<IOException> { NativeIroh.connect(endpoint, unusedNode, alpn, 1_000) }
    }

    @Test
    fun rejectsAMalformedNode() {
        val endpoint = NativeIroh.bind()
        try {
            assertFailsWith<IllegalArgumentException> { NativeIroh.connect(endpoint, "not a key", alpn, 1_000) }
        } finally {
            NativeIroh.closeEndpoint(endpoint)
        }
    }

    @Test
    fun anUnreachableNodeTimesOut() {
        val endpoint = NativeIroh.bind()
        try {
            assertFailsWith<IOException> { NativeIroh.connect(endpoint, unusedNode, alpn, 2_000) }
        } finally {
            NativeIroh.closeEndpoint(endpoint)
        }
    }

    /** 不认得的流编号读写都抛 [IOException]、关掉也没事，不会碰到别的流或释放了的内存。 */
    @Test
    fun unknownStreamsFailSafely() {
        val buffer = ByteArray(16)
        assertFailsWith<IOException> { NativeIroh.read(Long.MAX_VALUE, buffer, 0, buffer.size) }
        assertFailsWith<IOException> { NativeIroh.write(Long.MAX_VALUE, buffer, 0, buffer.size) }
        assertFailsWith<IndexOutOfBoundsException> { NativeIroh.read(Long.MAX_VALUE, buffer, 8, buffer.size) }
        NativeIroh.close(Long.MAX_VALUE)
    }

    private companion object {
        /** Ed25519 的基点，是个合法的公钥，没有哪台电脑拿它当地址。 */
        const val unusedNode = "5866666666666666666666666666666666666666666666666666666666666666"
    }
}
