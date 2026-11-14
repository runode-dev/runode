package dev.runode.mobile.connection

import dev.runode.mobile.protocol.CertificateFingerprint
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.Frame
import dev.runode.mobile.protocol.FrameKind
import dev.runode.mobile.protocol.GateMessage
import dev.runode.mobile.protocol.GateRejection
import dev.runode.mobile.protocol.HostMsg
import java.io.File
import java.nio.file.Files
import java.time.Instant
import java.util.UUID
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertTrue
import kotlin.time.Duration
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.Duration.Companion.seconds
import kotlin.time.TimeSource
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancelChildren
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.ReceiveChannel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.buffer
import kotlinx.coroutines.flow.produceIn
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.After
import org.junit.Test

/** 跑一个测试，最多 20 秒；[scope] 是外层的作用域，订阅事件的协程开在它上面，结束时一起取消。 */
fun test(body: suspend (scope: CoroutineScope) -> Unit): Unit = runBlocking {
    val outer = this
    try {
        withTimeout(20.seconds) { body(outer) }
    } finally {
        outer.coroutineContext.cancelChildren()
    }
}

/** 订阅事件：收集方不取就不往下拿，和界面跟不上时一样。 */
fun HostLink.subscribe(scope: CoroutineScope): ReceiveChannel<HostEvent> =
    events().buffer(Channel.RENDEZVOUS).produceIn(scope)

/** 等到 `ready`，返回它的 generation。 */
suspend fun ReceiveChannel<HostEvent>.waitReady(): ULong? {
    for (event in this) if (event is HostEvent.Ready) return event.generation
    return null
}

/** 等到满足 [predicate] 的状态。 */
suspend fun ReceiveChannel<HostEvent>.waitState(predicate: (LinkState) -> Boolean): LinkState? {
    for (event in this) if (event is HostEvent.State && predicate(event.state)) return event.state
    return null
}

private fun JsonObject.type(): String? = get("type")?.jsonPrimitive?.content

private fun secondsUntil(instant: Instant): Double = (instant.toEpochMilli() - System.currentTimeMillis()) / 1000.0

/** 连接：门禁加 `Hello`，事件的先后，发的东西按先后写出，断线重连，不能重试的失败停下。 */
class HostConnectionTests {
    private val machine = sampleMachine()
    private val keys = MemoryDeviceKeyStore().also { it.generate(machine.id) }
    private val store = MemoryMachineStore()
    private val supply = TransportSupply()
    private val links = mutableListOf<HostConnection>()

    @After
    fun closeLinks() = links.forEach { it.close() }

    private fun connection(
        policy: ReconnectPolicy = ReconnectPolicy(base = 10.milliseconds, maximum = 20.milliseconds),
        discovery: HostDiscovery = NoDiscovery,
        open: TransportOpener = supply.opener,
        tunnel: TunnelOpener? = null,
        networkChange: suspend () -> Unit = { awaitCancellation() },
        record: MachineRecord = machine,
    ) = HostConnection(
        machine = record,
        keyStore = keys,
        machines = store,
        discovery = discovery,
        identity = ClientIdentity(build = "android-test", deviceName = "测试 Pixel"),
        policy = policy,
        open = open,
        tunnel = tunnel,
        networkChange = networkChange,
    ).also { links += it }

    /** 扮演宿主走完门禁和握手，返回 `Hello` 的内容。 */
    private suspend fun admit(host: FakeHost, welcome: String = FakeHost.welcome): JsonObject {
        host.challenge()
        val auth = assertNotNull(host.readControl())
        assertEquals("remote_auth", auth.type())
        host.send(GateMessage.Accepted(machine.deviceId))
        val hello = assertNotNull(host.readControl())
        host.send(welcome)
        return hello
    }

    /** `keepAwake` 只发给在 `Welcome` 里说认得它的宿主：宿主对不认得的消息回不带编号的 `Error`，会被别的页面认错。 */
    private fun keepAwakeOnlyGoesToHostsThatKnowIt(knows: Boolean) = test { scope ->
        val transport = FakeTransport()
        supply.add(transport)
        val link = connection()
        val events = link.subscribe(scope)
        link.start()
        val host = FakeHost(transport)
        val welcome = if (knows) FakeHost.welcome.replace("}", ""","keep_awake":true}""") else FakeHost.welcome
        admit(host, welcome)
        assertNotNull(events.waitReady())
        link.send(ClientMsg.KeepAwake)
        link.send(ClientMsg.ListSessions)
        assertEquals(if (knows) "keep_awake" else "list_sessions", host.readControl()?.type())
        link.stop()
    }

    @Test
    fun keepAwakeIsNotSentToHostsThatDoNotKnowIt() = keepAwakeOnlyGoesToHostsThatKnowIt(false)

    @Test
    fun keepAwakeGoesToHostsThatKnowIt() = keepAwakeOnlyGoesToHostsThatKnowIt(true)

    /** `findCommands` 也只发给在 `Welcome` 里说认得它的宿主。 */
    private fun findCommandsOnlyGoesToHostsThatKnowIt(knows: Boolean) = test { scope ->
        val transport = FakeTransport()
        supply.add(transport)
        val link = connection()
        val events = link.subscribe(scope)
        link.start()
        val host = FakeHost(transport)
        val welcome = if (knows) FakeHost.welcome.replace("}", ""","shells":true}""") else FakeHost.welcome
        admit(host, welcome)
        assertNotNull(events.waitReady())
        link.send(ClientMsg.FindCommands(req = 1u, path = null, programs = listOf("claude")))
        link.send(ClientMsg.ListSessions)
        assertEquals(if (knows) "find_commands" else "list_sessions", host.readControl()?.type())
        link.stop()
    }

    @Test
    fun findCommandsIsNotSentToHostsThatDoNotKnowIt() = findCommandsOnlyGoesToHostsThatKnowIt(false)

    @Test
    fun findCommandsGoesToHostsThatKnowIt() = findCommandsOnlyGoesToHostsThatKnowIt(true)

    @Test
    fun connectsSaysHelloAndForwardsMessages() = test { scope ->
        // 连接开着时用户改了名字：记地址不能把名字改回去。
        store.upsert(machine.copy(name = "改过名"))
        val transport = FakeTransport()
        supply.add(transport)
        val link = connection()
        val events = link.subscribe(scope)
        link.start()
        val host = FakeHost(transport)
        val hello = admit(host)
        assertEquals("hello", hello.type())
        assertEquals("mobile", hello["client"]?.jsonPrimitive?.content)
        assertEquals("测试 Pixel", hello["device"]?.jsonPrimitive?.content)
        val caps = buildJsonObject {
            put("snapshot", JsonPrimitive(false))
            put("vt_replay", JsonPrimitive(true))
            put("shell_report", JsonPrimitive(true))
        }
        assertEquals(caps, hello["caps"])
        val generation = assertNotNull(events.waitReady())

        link.send(ClientMsg.ListSessions)
        assertEquals("list_sessions", host.readControl()?.type())
        // 旧连接的输入丢掉，当前的照常写出。
        link.sendInput("stale".encodeToByteArray(), channel = 1u, generation = generation + 1u)
        link.sendInput("ls\r".encodeToByteArray(), channel = 1u, generation = generation)
        val input = assertNotNull(host.readFrame())
        assertEquals(FrameKind.Input, input.kind)
        assertContentEquals("ls\r".encodeToByteArray(), input.payload)

        host.send("""{"type":"bell","id":"0123456789abcdef0011223344556677"}""")
        host.send(Frame(FrameKind.Output, 3u, "hi".encodeToByteArray()))
        var sawBell = false
        for (event in events) {
            if (event is HostEvent.Message && event.message is HostMsg.Bell) sawBell = true
            if (event is HostEvent.Frame) {
                assertContentEquals("hi".encodeToByteArray(), event.frame.payload)
                assertEquals(generation, event.generation)
                break
            }
        }
        assertTrue(sawBell)
        // 记下实际连上的地址。
        assertEquals("192.168.1.20", store.all().first().lastAddress)
        assertEquals("改过名", store.all().first().name)
        link.stop()
    }

    @Test
    fun helloCarriesTheRenamedDevice() = test {
        val transport = FakeTransport()
        supply.add(transport)
        val link = connection()
        link.setDeviceName("书房的 Pixel")
        link.start()
        val hello = admit(FakeHost(transport))
        assertEquals("书房的 Pixel", hello["device"]?.jsonPrimitive?.content)
        link.stop()
    }

    /** 电脑在挑战里报的跨网络地址记进电脑的记录，下次不在同一个网络时按它拨。 */
    @Test
    fun remembersTheNodeFromTheChallenge() = test { scope ->
        store.upsert(machine)
        val transport = FakeTransport()
        supply.add(transport)
        val link = connection()
        val events = link.subscribe(scope)
        link.start()
        val host = FakeHost(transport)
        host.challenge(node = node)
        host.readControl()
        host.send(GateMessage.Accepted(machine.deviceId))
        host.readControl()
        host.send(FakeHost.welcome)
        assertNotNull(events.waitReady())
        assertEquals(node, store.all().first().node)
        link.stop()
    }

    /** 记着的地址都连不上时经 iroh 按公钥拨过去，门禁照常走。 */
    @Test
    fun fallsBackToTheTunnel() = test { scope ->
        val tunnelled = FakeTransport(remoteAddress = null)
        val dialed = mutableListOf<String>()
        val link = connection(
            record = machine.copy(node = node),
            tunnel = { node, _ ->
                synchronized(dialed) { dialed += node }
                tunnelled
            },
        )
        val events = link.subscribe(scope)
        link.start()
        admit(FakeHost(tunnelled))
        assertNotNull(events.waitReady())
        assertEquals(listOf(node), synchronized(dialed) { dialed.toList() })
        link.stop()
    }

    @Test
    fun reconnectsAfterTheHostGoesAway() = test { scope ->
        val first = FakeTransport()
        val second = FakeTransport()
        supply.add(first)
        supply.add(second)
        val link = connection()
        val events = link.subscribe(scope)
        link.start()
        admit(FakeHost(first))
        val generation = assertNotNull(events.waitReady())
        first.close()
        admit(FakeHost(second))
        assertEquals(generation + 1u, events.waitReady())
        link.stop()
    }

    /**
     * `unknown_device`（电脑上撤销了这台设备）不自动重连：不换下一个地址、不按退避重试，App 回到前台再 `start` 也不连；
     * 只有用户点重试（`reconnectNow`）才再试一次。电脑把这类失败计入限速。
     */
    @Test
    fun aRevokedDeviceStopsRetrying() = test { scope ->
        val transport = FakeTransport()
        supply.add(transport)
        val link = connection()
        val events = link.subscribe(scope)
        link.start()
        val host = FakeHost(transport)
        host.challenge()
        host.readControl()
        host.send(GateMessage.Rejected(GateRejection.UnknownDevice))
        val failed = events.waitState { it is LinkState.Failed } as? LinkState.Failed
        assertEquals(LinkFailure.Rejected(GateRejection.UnknownDevice), failed?.failure)
        // 退避只有 10 毫秒，等它十几倍的时间也没有再连。
        delay(150.milliseconds)
        assertEquals(listOf("192.168.1.20"), supply.requestedTargets)
        // App 回到前台时会再 `start`：照样不连。
        link.start()
        delay(150.milliseconds)
        assertEquals(listOf("192.168.1.20"), supply.requestedTargets)
        // 用户点了重试才再连。
        val retry = FakeTransport()
        supply.add(retry)
        link.reconnectNow()
        admit(FakeHost(retry))
        assertNotNull(events.waitReady())
        assertEquals(listOf("192.168.1.20", "192.168.1.20"), supply.requestedTargets)
        link.stop()
    }

    /** 握手中途 `stop`（App 退到后台）：卡在门禁上读的那一步也要停下，回到 `idle`，不一直停在「连接中」。 */
    @Test
    fun stoppingDuringTheHandshakeGivesUp() = test { scope ->
        val transport = FakeTransport()
        supply.add(transport)
        val link = connection()
        val states = mutableListOf<LinkState>()
        scope.launch {
            link.events().collect { event -> if (event is HostEvent.State) synchronized(states) { states += event.state } }
        }
        link.start()
        // 宿主一直不发 challenge：连接开好以后停。
        while (supply.requestedTargets.isEmpty()) delay(5.milliseconds)
        link.stop()
        // 门禁的超时是 10 秒，两秒内回到 `idle` 说明是取消让它停下的。
        fun stopped() = synchronized(states) { states.dropWhile { it != LinkState.Connecting }.contains(LinkState.Idle) }
        repeat(200) { if (!stopped()) delay(10.milliseconds) }
        assertTrue(stopped())
    }

    @Test
    fun onlyRateLimitsAreRetried() {
        for (reason in listOf(GateRejection.UnknownDevice, GateRejection.BadSignature, GateRejection.PairingInvalid)) {
            assertTrue(LinkFailure.Rejected(reason).isFatal, "$reason")
        }
        assertFalse(LinkFailure.Rejected(GateRejection.RateLimited).isFatal)
        assertFalse(LinkFailure.Rejected(GateRejection.Disabled).isFatal)
        assertFalse(LinkFailure.Rejected(GateRejection.Unknown("future")).isFatal)
        assertFalse(LinkFailure.Timeout.isFatal)
        // 连不上和远程访问没开算电脑离线：等它上线，不按退避一遍遍试。
        for (failure in listOf(
            LinkFailure.Timeout,
            LinkFailure.NoAddress,
            LinkFailure.ConnectionFailed("refused"),
            LinkFailure.Rejected(GateRejection.Disabled),
        )) {
            assertTrue(failure.isOffline, "$failure")
        }
        assertFalse(LinkFailure.Rejected(GateRejection.RateLimited).isOffline)
        assertFalse(LinkFailure.Closed("bye").isOffline)
        assertFalse(LinkFailure.HostClosed.isOffline)
    }

    /** 电脑离线（连不上）时不按退避一遍遍试：等它在局域网里重新公布服务，公布了马上连。 */
    @Test
    fun anOfflineHostIsNotPolledUntilItReappears() = test { scope ->
        val discovery = SignalDiscovery()
        val link = connection(
            policy = ReconnectPolicy(base = 10.milliseconds, maximum = 20.milliseconds, offlineRetry = 30.seconds),
            discovery = discovery,
        )
        val events = link.subscribe(scope)
        link.start()
        // 没有准备连接，连不上：进入等待，下次兜底的重试在很久以后。
        val waiting = events.waitState { it is LinkState.Waiting && secondsUntil(it.retryAt) > 10 } as? LinkState.Waiting
        assertEquals(true, waiting?.offline)
        val attempts = supply.requestedTargets.size
        delay(200.milliseconds)
        assertEquals(attempts, supply.requestedTargets.size)
        // 电脑回来了：公布服务，连接马上再试。
        val transport = FakeTransport()
        supply.add(transport)
        val started = TimeSource.Monotonic.markNow()
        discovery.appear()
        admit(FakeHost(transport))
        assertNotNull(events.waitReady())
        assertTrue(started.elapsedNow() < 2.seconds)
        link.stop()
    }

    /** 连不上、服务却还公布着（电脑睡着、睡眠代理替它公布）：不按退避一遍遍连，也不干等兜底的重试，按 `idleMaximum` 隔一会儿试一次。 */
    @Test
    fun aPublishedButUnreachableHostIsRetriedAtTheIdlePace() = test { scope ->
        val published = object : HostDiscovery {
            override suspend fun locate(fingerprint: CertificateFingerprint, timeout: Duration): TransportTarget? =
                TransportTarget.address("192.168.1.30", 7290u)
        }
        val link = connection(
            policy = ReconnectPolicy(
                base = 10.milliseconds,
                maximum = 20.milliseconds,
                idleMaximum = 800.milliseconds,
                offlineRetry = 30.seconds,
            ),
            discovery = published,
        )
        val events = link.subscribe(scope)
        link.start()
        val waiting = events.waitState { it is LinkState.Waiting } as? LinkState.Waiting
        assertEquals(true, waiting?.offline)
        val attempts = supply.requestedTargets.size
        delay(300.milliseconds)
        assertEquals(attempts, supply.requestedTargets.size)
        val transport = FakeTransport()
        supply.add(transport)
        val started = TimeSource.Monotonic.markNow()
        admit(FakeHost(transport))
        assertNotNull(events.waitReady())
        assertTrue(started.elapsedNow() < 5.seconds)
        link.stop()
    }

    /** 经记着的地址连的电脑离线（没有 Bonjour 可等）：本机网络恢复或变化时马上再试，不等兜底的重试。 */
    @Test
    fun anOfflineHostIsRetriedWhenTheNetworkChanges() = test { scope ->
        val network = NetworkSignal()
        val link = connection(
            policy = ReconnectPolicy(base = 10.milliseconds, maximum = 20.milliseconds, offlineRetry = 30.seconds),
            networkChange = { network.await() },
        )
        val events = link.subscribe(scope)
        link.start()
        val waiting = events.waitState { it is LinkState.Waiting && secondsUntil(it.retryAt) > 10 } as? LinkState.Waiting
        assertEquals(true, waiting?.offline)
        val attempts = supply.requestedTargets.size
        delay(200.milliseconds)
        assertEquals(attempts, supply.requestedTargets.size)
        val transport = FakeTransport()
        supply.add(transport)
        val started = TimeSource.Monotonic.markNow()
        network.change()
        admit(FakeHost(transport))
        assertNotNull(events.waitReady())
        assertTrue(started.elapsedNow() < 2.seconds)
        link.stop()
    }

    /** 网络接连变化（换接口、连 VPN）时也守着退避：离上次失败不到退避的时长不再试，到了才试。 */
    @Test
    fun networkChangesDoNotBypassTheBackoff() = test { scope ->
        val network = NetworkSignal()
        val link = connection(
            policy = ReconnectPolicy(base = 600.milliseconds, maximum = 600.milliseconds, offlineRetry = 30.seconds),
            networkChange = { network.await() },
        )
        val events = link.subscribe(scope)
        link.start()
        val waiting = events.waitState { it is LinkState.Waiting } as? LinkState.Waiting
        assertEquals(true, waiting?.offline)
        val attempts = supply.requestedTargets.size
        delay(20.milliseconds)
        val started = TimeSource.Monotonic.markNow()
        network.change()
        delay(200.milliseconds)
        assertEquals(attempts, supply.requestedTargets.size)
        val transport = FakeTransport()
        supply.add(transport)
        admit(FakeHost(transport))
        assertNotNull(events.waitReady())
        // 是那次网络变化在退避过后叫醒的，不是兜底的重试。
        assertTrue(started.elapsedNow() < 2.seconds)
        link.stop()
    }

    @Test
    fun triesTheNextAddressWhenOneFails() = test { scope ->
        val transport = FakeTransport()
        // 第一个地址打不开（没有准备连接），第二个连上。
        val opener = supply.opener
        val flaky = connection(open = { target, fingerprint ->
            if (target.label == "192.168.1.20") throw LinkFailure.ConnectionFailed("refused")
            opener(target, fingerprint)
        })
        supply.add(transport)
        val events = flaky.subscribe(scope)
        flaky.start()
        admit(FakeHost(transport))
        assertNotNull(events.waitReady())
        assertEquals(listOf("fd7a::1"), supply.requestedTargets)
        flaky.stop()
    }

    /** 组播不通、Bonjour 一直找不到时，记着的地址照常马上连，不等 Bonjour 的超时。 */
    @Test
    fun storedAddressesDoNotWaitForBonjour() = test { scope ->
        val slow = object : HostDiscovery {
            override suspend fun locate(fingerprint: CertificateFingerprint, timeout: Duration): TransportTarget? {
                delay(30.seconds)
                return null
            }
        }
        val link = connection(discovery = slow)
        val transport = FakeTransport()
        supply.add(transport)
        val events = link.subscribe(scope)
        val started = TimeSource.Monotonic.markNow()
        link.start()
        admit(FakeHost(transport))
        assertNotNull(events.waitReady())
        assertTrue(started.elapsedNow() < 5.seconds)
        link.stop()
    }

    /** 一次读到的帧里，同一通道上连着的输出拼成一帧交出去；控制消息和别的通道把它隔开，先后不变。 */
    @Test
    fun mergesConsecutiveOutputFrames() = test { scope ->
        val link = connection()
        val transport = FakeTransport()
        supply.add(transport)
        val events = link.subscribe(scope)
        link.start()
        admit(FakeHost(transport))
        assertNotNull(events.waitReady())
        fun output(channel: UInt, text: String) = Frame(FrameKind.Output, channel, text.encodeToByteArray()).encoded()
        val bell = Frame(
            FrameKind.Control,
            0u,
            """{"type":"bell","id":"0123456789abcdef0011223344556677"}""".encodeToByteArray(),
        ).encoded()
        transport.inbound.push(output(1u, "ab") + output(1u, "cd") + bell + output(1u, "ef") + output(2u, "gh"))
        val seen = mutableListOf<String>()
        for (event in events) {
            when {
                event is HostEvent.Frame -> seen += "${event.frame.channel}:${event.frame.payload.decodeToString()}"
                event is HostEvent.Message && event.message is HostMsg.Bell -> seen += "bell"
            }
            if (seen.size == 4) break
        }
        assertEquals(listOf("1:abcd", "bell", "1:ef", "2:gh"), seen)
        link.stop()
    }

    /** 订阅者不取输出、积压过了上限时不再往下读，积压留在网络和宿主那边；取走以后接着读，一帧不丢。 */
    @Test
    fun stopsReadingWhileASubscriberFallsBehind() = test { scope ->
        val link = connection()
        val transport = FakeTransport()
        supply.add(transport)
        val events = link.subscribe(scope)
        link.start()
        admit(FakeHost(transport))
        assertNotNull(events.waitReady())
        val chunk = Frame(FrameKind.Output, 1u, ByteArray(64 shl 10)).encoded()
        repeat(16) { transport.inbound.push(chunk) }
        delay(200.milliseconds)
        assertTrue(transport.inbound.pending > 8, "pending ${transport.inbound.pending}")
        var frames = 0
        for (event in events) {
            if (event is HostEvent.Frame) frames += 1
            if (frames == 16) break
        }
        assertEquals(16, frames)
        assertEquals(0, transport.inbound.pending)
        link.stop()
    }

    @Test
    fun missingKeyIsFatal() = test { scope ->
        keys.deleteKey(machine.id)
        val link = connection()
        val events = link.subscribe(scope)
        link.start()
        val failed = events.waitState { it is LinkState.Failed } as? LinkState.Failed
        assertEquals(LinkFailure.MissingKey, failed?.failure)
    }

    @Test
    fun backoffGrowsAndCaps() {
        val policy = ReconnectPolicy(base = 1.seconds, maximum = 15.seconds)
        assertEquals(Duration.ZERO, policy.delay(afterFailures = 0))
        assertEquals(1.seconds, policy.delay(afterFailures = 1))
        assertEquals(4.seconds, policy.delay(afterFailures = 3))
        assertEquals(15.seconds, policy.delay(afterFailures = 10))
        assertEquals(4.seconds, policy.delay(afterFailures = 3, focused = false))
        assertEquals(32.seconds, policy.delay(afterFailures = 6, focused = false))
        assertEquals(60.seconds, policy.delay(afterFailures = 10, focused = false))
    }

    /** 不在看的电脑连不上时退避到 `idleMaximum`；变回在看时不等这么久，马上重试。 */
    @Test
    fun focusingAnIdleMachineRetriesAtOnce() = test { scope ->
        // 第一次失败就等 5 秒，比 `maximum` 长得多，测试不用等它一档档涨上去。
        val link = connection(
            policy = ReconnectPolicy(base = 5.seconds, maximum = 20.milliseconds, idleMaximum = 30.seconds),
        )
        link.setFocused(false)
        val events = link.subscribe(scope)
        link.start()
        // 没有准备连接，连不上。
        events.waitState { it is LinkState.Waiting && secondsUntil(it.retryAt) > 2 }
        val transport = FakeTransport()
        supply.add(transport)
        val started = TimeSource.Monotonic.markNow()
        link.setFocused(true)
        admit(FakeHost(transport))
        assertNotNull(events.waitReady())
        assertTrue(started.elapsedNow() < 2.seconds)
        link.stop()
    }

    private companion object {
        const val node = "ae58ff8833241ac82d6ff7611046ed67b5072d142c588d0063e942d9a75502b6"
    }
}

class MachineTests {
    private val directories = mutableListOf<File>()

    @After
    fun removeDirectories() = directories.forEach { it.deleteRecursively() }

    private fun temporaryFile(): File =
        Files.createTempDirectory("runode-test-").toFile().also { directories += it }.resolve("machines.json")

    @Test
    fun fallbackTargetsPutTheLastAddressFirst() {
        val machine = sampleMachine().copy(lastAddress = "fd7a::1")
        assertEquals(listOf("fd7a::1", "192.168.1.20"), machine.fallbackTargets.map { it.label })
    }

    @Test
    fun fileStoreRoundTrips() = runBlocking {
        val file = temporaryFile()
        val store = FileMachineStore(file)
        var machine = sampleMachine().copy(node = "abc")
        store.upsert(machine)
        machine = machine.copy(name = "改过名")
        store.upsert(machine)
        assertEquals(listOf(machine), FileMachineStore(file).all())
        store.remove(machine.id)
        assertTrue(FileMachineStore(file).all().isEmpty())
    }

    @Test
    fun updatingTheAddressKeepsTheNewName() = runBlocking {
        val file = temporaryFile()
        val store = FileMachineStore(file)
        var machine = sampleMachine()
        store.upsert(machine)
        machine = machine.copy(name = "改过名")
        store.upsert(machine)
        store.updateLastAddress(machine.id, "fd7a::1")
        assertEquals(listOf(machine.copy(lastAddress = "fd7a::1")), FileMachineStore(file).all())
    }

    @Test
    fun updatingTheAddressOfADeletedMachineWritesNothing() = runBlocking {
        val file = temporaryFile()
        val store = FileMachineStore(file)
        val machine = sampleMachine()
        store.upsert(machine)
        store.remove(machine.id)
        store.updateLastAddress(machine.id, "fd7a::1")
        assertTrue(FileMachineStore(file).all().isEmpty())

        val memory = MemoryMachineStore()
        memory.updateLastAddress(machine.id, "fd7a::1")
        assertTrue(memory.all().isEmpty())
    }

    @Test
    fun anUnreadableStoreIsNotOverwritten() = runBlocking {
        val file = temporaryFile()
        val first = sampleMachine()
        FileMachineStore(file).upsert(first)
        // 读不出来时，加一台不能把文件换成只剩新的那台。
        assertTrue(file.setReadable(false))
        val store = FileMachineStore(file)
        assertTrue(store.all().isEmpty())
        assertFailsWith<Exception> { store.upsert(sampleMachine()) }
        assertFailsWith<Exception> { store.remove(first.id) }
        assertTrue(file.setReadable(true))
        assertEquals(listOf(first), FileMachineStore(file).all())
    }

    /** 解不出来的文件挪开留底，不当成空表盖掉。 */
    @Test
    fun aCorruptStoreIsSetAside() = runBlocking {
        val file = temporaryFile()
        file.writeText("not json")
        assertTrue(FileMachineStore(file).all().isEmpty())
        assertEquals("not json", File(file.path + ".corrupt").readText())
        assertFalse(file.exists())
    }
}

/** 测试代替本机网络监听：[change] 叫醒正在等的。 */
class NetworkSignal {
    private val waiters = mutableListOf<CompletableDeferred<Unit>>()

    suspend fun await() {
        val waiter = CompletableDeferred<Unit>()
        synchronized(waiters) { waiters += waiter }
        waiter.await()
    }

    fun change() {
        val woken = synchronized(waiters) { waiters.toList().also { waiters.clear() } }
        woken.forEach { it.complete(Unit) }
    }
}

/** 找不到电脑，直到测试调 [appear]：那之后 [awaitPresence] 立刻返回。 */
class SignalDiscovery : HostDiscovery {
    private val present = CompletableDeferred<Unit>()

    override suspend fun locate(fingerprint: CertificateFingerprint, timeout: Duration): TransportTarget? = null

    override suspend fun awaitPresence(fingerprint: CertificateFingerprint) = present.await()

    fun appear() {
        present.complete(Unit)
    }
}

/** 测的是 [UUID] 记录的写法，免得改了序列化器以后读不出旧文件。 */
class MachineRecordFormatTests {
    @Test
    fun recordsKeepTheirIdAndTimes() {
        val id = UUID.fromString("6f1c2a3b-4d5e-4f60-8a9b-0c1d2e3f4a5b")
        val machine = sampleMachine(id).copy(pairedAt = Instant.parse("2026-11-14T08:00:00Z"))
        val text = kotlinx.serialization.json.Json.encodeToString(MachineRecord.serializer(), machine)
        assertTrue(""""id":"6f1c2a3b-4d5e-4f60-8a9b-0c1d2e3f4a5b"""" in text, text)
        assertTrue(""""pairedAt":"2026-11-14T08:00:00Z"""" in text, text)
        assertEquals(machine, kotlinx.serialization.json.Json.decodeFromString(MachineRecord.serializer(), text))
    }
}
