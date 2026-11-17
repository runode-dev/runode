package dev.runode.mobile.features.model

import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.protocol.SessionId
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Rule

/** 终端页开着时发 `KeepAwake`，只在连接连上时发：App 进了后台连接停掉，或者断开等重连时，不按间隔空转。 */
@OptIn(ExperimentalCoroutinesApi::class)
class KeepAwakeTests {
    @get:Rule val main = MainDispatcherRule()

    private val link = FakeLink()
    private val session = SessionId("11111111111111111111111111111111")!!
    private val interval = TerminalModel.keepAwakeInterval

    @Test fun sendsOnlyWhileTheLinkIsConnected() = runTest {
        val model = TerminalModel(session, UiText.Raw("zsh"), link = link) {}
        // 断言失败也要关掉：开着的循环会让 runTest 收尾时一直推进虚拟时间。
        try {
            sendsOnlyWhileConnected(model)
        } finally {
            model.close()
        }
    }

    private fun TestScope.sendsOnlyWhileConnected(model: TerminalModel) {
        model.open()
        advanceTimeBy(interval * 5)
        runCurrent()
        assertEquals(0, link.keepAwakes)

        link.state.value = LinkState.Connected("Mac mini", null)
        advanceTimeBy(interval * 3)
        runCurrent()
        assertTrue(link.keepAwakes >= 3, "keepAwakes ${link.keepAwakes}")

        // App 进后台时连接停掉，回到 Idle。
        link.state.value = LinkState.Idle
        advanceTimeBy(interval)
        runCurrent()
        val paused = link.keepAwakes
        advanceTimeBy(interval * 10)
        runCurrent()
        assertEquals(paused, link.keepAwakes)

        link.state.value = LinkState.Connected("Mac mini", null)
        runCurrent()
        assertEquals(paused + 1, link.keepAwakes)
    }
}
