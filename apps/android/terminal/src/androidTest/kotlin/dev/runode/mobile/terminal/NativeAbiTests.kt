package dev.runode.mobile.terminal

import androidx.test.ext.junit.runners.AndroidJUnit4
import kotlin.test.assertContentEquals
import org.junit.Test
import org.junit.runner.RunWith

/** Kotlin 照抄的布局和 libghostty 取值与 C 那边逐项相等。 */
@RunWith(AndroidJUnit4::class)
class NativeAbiTests {
    @Test
    fun snapshotLayoutMatchesTheNativeOne() {
        assertContentEquals(VtJni.nativeLayout(), SnapshotLayout.signature)
    }

    @Test
    fun ghosttyConstantsMatchTheHeaders() {
        assertContentEquals(VtJni.nativeGhosttyConstants(), GhosttyAbi.signature)
    }
}
