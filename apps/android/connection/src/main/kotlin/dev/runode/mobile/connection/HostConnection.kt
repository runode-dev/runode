package dev.runode.mobile.connection

import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.Frame
import dev.runode.mobile.protocol.FrameDecoder
import dev.runode.mobile.protocol.FrameError
import dev.runode.mobile.protocol.FrameKind
import dev.runode.mobile.protocol.GateRejection
import dev.runode.mobile.protocol.GoodbyeReason
import dev.runode.mobile.protocol.HostMsg
import java.io.ByteArrayOutputStream
import java.time.Instant
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import kotlin.coroutines.cancellation.CancellationException
import kotlin.time.Duration
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.Duration.Companion.seconds
import kotlin.time.TimeMark
import kotlin.time.TimeSource
import kotlin.time.toKotlinDuration
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancel
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.selects.select
import kotlinx.coroutines.withContext

/**
 * 断线重连等多久：第 n 次连续失败后等 `base * 2^(n-1)`，最多 [maximum]；用户没在看的电脑（[HostLink.setFocused]）最多
 * [idleMaximum]。电脑离线（连不上、没开远程访问）时不按这个退避一遍遍试，等它在局域网里重新公布 Bonjour 服务、或者本机
 * 网络恢复或变化时再连，兜底每 [offlineRetry] 探一次（经记着的地址连的电脑没有 Bonjour 可等，网络恢复、VPN 连好时要靠
 * 后者）。
 */
data class ReconnectPolicy(
    val base: Duration = 1.seconds,
    val maximum: Duration = 15.seconds,
    val idleMaximum: Duration = 60.seconds,
    val offlineRetry: Duration = 300.seconds,
) {
    fun delay(afterFailures: Int, focused: Boolean = true): Duration {
        if (afterFailures <= 0) return Duration.ZERO
        val factor = 1 shl minOf(afterFailures - 1, 16)
        return minOf(base * factor, if (focused) maximum else maxOf(maximum, idleMaximum))
    }
}

/**
 * 一台电脑上宿主的连接：找地址、TLS、门禁、`Hello`，之后收发帧，断了自动重连。
 *
 * 线程模型：连接的状态（当前的连接、代数、要不要连、重连的协程）只在 [confined] 上读写，它是
 * [Dispatchers.Default] 上一次只跑一个任务的视图，相当于一个 actor；挂起的公开方法先切到它上面再动状态。
 * [send]、[sendInput] 排进无界的 [outbox]，由它上面的写协程按先后写出；[setFocused]、[nextRequestId] 只碰原子量，
 * 哪个线程调都行。每个订阅者有一条无界的事件队列，[events] 的收集方在自己的协程里取。阻塞的网络读写不在
 * [confined] 上，在 [FrameTransport] 的实现里。
 *
 * 用完调 [close]：它停掉所有协程，之后这个对象不能再用。
 */
class HostConnection(
    machine: MachineRecord,
    private val keyStore: DeviceKeyStore,
    private val machines: MachineStore,
    private val discovery: HostDiscovery,
    private var identity: ClientIdentity,
    private val policy: ReconnectPolicy = ReconnectPolicy(),
    private val open: TransportOpener = { target, fingerprint -> TlsChannel.open(target, fingerprint) },
    /** 跨网络的那条路；为空时只在局域网里连。 */
    private val tunnel: TunnelOpener? = { node, fingerprint -> IrohDialer.shared.open(node, fingerprint) },
    /** 电脑离线时除了等 Bonjour，也等本机网络恢复或变化（[NetworkChanges.await]）；默认不等。 */
    private val networkChange: suspend () -> Unit = { awaitCancellation() },
) : HostLink {
    private var machine = machine
    private val confined = Dispatchers.Default.limitedParallelism(1)
    private val scope = CoroutineScope(SupervisorJob() + confined)

    private val subscribers = ConcurrentHashMap<Long, Subscriber>()
    private val nextSubscriber = AtomicLong()
    private val mutableState = MutableStateFlow<LinkState>(LinkState.Idle)
    override val state: StateFlow<LinkState> = mutableState.asStateFlow()

    /** 门禁和握手都过了的连接；没连上时为空，排着的消息就丢掉。 */
    private var transport: FrameTransport? = null

    /** 这次连上的宿主认得 [ClientMsg.KeepAwake]、[ClientMsg.FindCommands]；不认得的宿主不发给它。 */
    private var hostKeepsAwake = false
    private var hostFindsCommands = false
    private var generation: ULong = 0u

    /** 上层要不要连接：[start] 置真，[stop] 和不能重试的失败置假。 */
    private var wanted = false
    private var runJob: Job? = null
    private var backoffJob: Job? = null
    private var writerJob: Job? = null
    private val requestLock = Any()
    private var requestCounter: UInt = 0u

    /** 用户在看这台电脑。 */
    private val focused = AtomicBoolean(true)

    /** 发出去的东西先排在这里，由 [writerJob] 按先后写；[send] 因此不用挂起。 */
    private val outbox = Channel<Outgoing>(Channel.UNLIMITED)

    private sealed class Outgoing {
        class Message(val message: ClientMsg) : Outgoing()

        class Input(val data: ByteArray, val channel: UInt, val generation: ULong) : Outgoing()
    }

    // HostLink

    /** 收集方取走一帧输出时从它的积压里减掉，读的一方据此知道它积压了多少。 */
    override fun events(): Flow<HostEvent> = flow {
        val id = nextSubscriber.getAndIncrement()
        val subscriber = Subscriber()
        withContext(confined) {
            subscribers[id] = subscriber
            subscriber.events.trySend(HostEvent.State(mutableState.value))
            if (transport != null) subscriber.events.trySend(HostEvent.Ready(generation))
        }
        try {
            for (event in subscriber.events) {
                if (event is HostEvent.Frame) subscriber.backlog.addAndGet(-event.frame.payload.size)
                emit(event)
            }
        } finally {
            subscribers.remove(id)
        }
    }

    override fun send(message: ClientMsg) {
        outbox.trySend(Outgoing.Message(message))
    }

    override fun sendInput(data: ByteArray, channel: UInt, generation: ULong) {
        outbox.trySend(Outgoing.Input(data, channel, generation))
    }

    /**
     * 要连接。上次是不能重试的失败（比如 `unknown_device`：电脑上撤销了这台设备）时不自动再连：再试也一样，其中签名
     * 不对、口令作废电脑还会计入限速。要用户点「重试」（[reconnectNow]）才再试。
     */
    override suspend fun start() = withContext(confined) { startHere() }

    override suspend fun stop(): Unit = withContext(confined) {
        wanted = false
        runJob?.cancel()
        backoffJob?.cancel()
        transport?.close()
    }

    /** 用户要立刻重试：等着重连的马上试，停在不能重试的失败上的也重新开始。 */
    override suspend fun reconnectNow(): Unit = withContext(confined) {
        if (mutableState.value is LinkState.Failed) setState(LinkState.Idle)
        if (runJob == null) startHere() else backoffJob?.cancel()
    }

    /**
     * 不在看的电脑连不上时最多等 [ReconnectPolicy.idleMaximum] 再试：首页上连着所有配对过的电脑，关着的那几台一遍遍
     * 找 Bonjour、挨个试地址太费电。变回在看时，还要等比 [ReconnectPolicy.maximum] 久的马上重试。
     */
    override fun setFocused(focused: Boolean) {
        val old = this.focused.getAndSet(focused)
        if (focused && !old) scope.launch { focusGained() }
    }

    /** 下次握手起在 `Hello` 里报新的设备名；这次连接上已经报过的不改。 */
    override suspend fun setDeviceName(name: String) = withContext(confined) {
        identity = identity.copy(deviceName = name)
    }

    override fun nextRequestId(): UInt = synchronized(requestLock) {
        requestCounter += 1u
        if (requestCounter == 0u) requestCounter = 1u
        requestCounter
    }

    /** 不再用这条连接：停掉重连、读写的协程，结束各订阅者的事件流。 */
    fun close() {
        scope.cancel()
        outbox.close()
        subscribers.values.forEach { it.events.close() }
    }

    // 连接

    private fun startHere() {
        if (mutableState.value is LinkState.Failed) return
        wanted = true
        if (writerJob == null) {
            writerJob = scope.launch {
                for (item in outbox) write(item)
            }
        }
        if (runJob == null) runJob = scope.launch { run() } else backoffJob?.cancel()
    }

    private fun focusGained() {
        val waiting = mutableState.value as? LinkState.Waiting ?: return
        if (!focused.get()) return
        if (java.time.Duration.between(Instant.now(), waiting.retryAt).toKotlinDuration() > policy.maximum) {
            backoffJob?.cancel()
        }
    }

    private fun broadcast(event: HostEvent) {
        val bytes = (event as? HostEvent.Frame)?.frame?.payload?.size ?: 0
        for (subscriber in subscribers.values) {
            if (bytes > 0) subscriber.backlog.addAndGet(bytes)
            subscriber.events.trySend(event)
        }
    }

    private fun setState(new: LinkState) {
        if (new == mutableState.value) return
        mutableState.value = new
        broadcast(HostEvent.State(new))
    }

    /** 连接的主循环：连上、读到断开、等一会儿再连，直到不要了或者遇到不能重试的失败。 */
    private suspend fun run() {
        var failures = 0
        try {
            while (wanted && currentCoroutineContext().isActive) {
                setState(LinkState.Connecting)
                // 这次是电脑离线（连不上、没开远程访问），不是连上后断的或别的错。
                var offline = false
                val reason: LinkFailure = try {
                    val session = establish()
                    // 等握手收尾时 `stop()` 插了进来：那时还没有 `transport` 可关，这里关掉，不当成连上了。
                    if (!wanted || !currentCoroutineContext().isActive) {
                        session.transport.close()
                        break
                    }
                    val connectedAt = TimeSource.Monotonic.markNow()
                    generation += 1u
                    transport = session.transport
                    hostKeepsAwake = session.keepAwake
                    hostFindsCommands = session.findsCommands
                    setState(LinkState.Connected(session.hostName, session.transport.remoteAddress))
                    broadcast(HostEvent.Ready(generation))
                    val closed = readUntilClosed(session.transport, session.decoder)
                    transport = null
                    session.transport.close()
                    // 连了一阵才断的（比如宿主交接给新版本）马上重连，重连失败了从第一档退避起；一连上就断的接着退避，
                    // 免得宿主过了门禁就关时来回撞。
                    failures = if (connectedAt.elapsedNow() > 10.seconds) 0 else failures + 1
                    closed
                } catch (failure: LinkFailure) {
                    if (failure.isFatal) {
                        wanted = false
                        setState(LinkState.Failed(failure))
                        break
                    }
                    failures += 1
                    // 被限速了：直接等最长的退避，不在限速期里接着撞。
                    if (failure == LinkFailure.Rejected(GateRejection.RateLimited)) failures = maxOf(failures, 16)
                    offline = failure.isOffline
                    failure
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (error: Exception) {
                    failures += 1
                    LinkFailure.Closed(error.message ?: error.toString())
                }
                if (!wanted || !currentCoroutineContext().isActive) break
                val delay = policy.delay(failures, focused.get())
                // 电脑离线时不一遍遍试：退避过后等它在局域网里重新公布服务，本机网络恢复或变化时也再试（但离上次失败
                // 不短于退避：换接口、连 VPN 时网络会接连变好几次，不能每变一次都连一回），兜底到 `offlineRetry` 再探一次。
                // 用户点重试、回到这台电脑的页面（`focusGained`）照样马上试。
                val retryAfter = if (offline) maxOf(delay, policy.offlineRetry) else delay
                setState(LinkState.Waiting(reason, Instant.now().plusMillis(retryAfter.inWholeMilliseconds), offline))
                val failedAt = TimeSource.Monotonic.markNow()
                coroutineScope {
                    backoffJob = launch { backoff(offline, delay, retryAfter, failedAt) }
                }
                backoffJob = null
            }
        } finally {
            transport = null
            runJob = null
            if (mutableState.value !is LinkState.Failed) setState(LinkState.Idle)
            // `stop` 之后又 `start` 了：接着跑。
            if (wanted) startHere()
        }
    }

    /** 等到该重试的时候；被取消（用户点重试、回到这台电脑的页面）时提前结束。 */
    private suspend fun backoff(offline: Boolean, delay: Duration, retryAfter: Duration, failedAt: TimeMark) {
        if (!offline) return delay(delay)
        val fingerprint = machine.fingerprint
        val earliest = failedAt + delay
        firstOf(
            {
                // 连不上时它还公布着服务（电脑睡着、睡眠代理替它公布，或者组播通、连接被挡）：等「公布」会立刻返回，
                // 退化成按退避一遍遍连；电脑醒来时记录也可能不变，干等 `offlineRetry` 又太久。所以这时按 `idleMaximum`
                // 试一次，比退避稀，醒来后也不久就连上。
                val published = discovery.locate(fingerprint, 2.seconds) != null
                delayUntil(earliest)
                if (published) delayUntil(failedAt + policy.idleMaximum) else discovery.awaitPresence(fingerprint)
            },
            {
                networkChange()
                delayUntil(earliest)
            },
            { delay(retryAfter) },
        )
    }

    private class Session(
        val transport: FrameTransport,
        val decoder: FrameDecoder,
        val hostName: String,
        val keepAwake: Boolean,
        val findsCommands: Boolean,
    )

    /** 经 [dial] 连上这台电脑，在连上的那条连接上走门禁、`Hello` 和 `Welcome`。 */
    private suspend fun establish(): Session =
        dial(machine, keyStore, discovery, open, tunnel, handshake = ::admit, close = { it.transport.close() })

    /** 在开好的连接上走门禁、`Hello` 和 `Welcome`。 */
    private suspend fun admit(transport: FrameTransport, key: DeviceKey): Session {
        try {
            val outcome = GateClient.run(transport, GateCredential.Auth(machine.deviceId, key))
            transport.send(Frame.control(identity.hello).encoded())
            val reader = FrameReader(transport, outcome.decoder)
            val welcome = withLinkTimeout(GateClient.timeout, onTimeout = transport::close) { readWelcome(reader) }
            val node = outcome.node
            if (node != null && node != machine.node) {
                machine = machine.copy(node = node)
                ignoringErrors { machines.updateNode(machine.id, node) }
            }
            val address = transport.remoteAddress
            if (address != null && address != machine.lastAddress) {
                machine = machine.copy(lastAddress = address)
                // 只改地址：`machine` 是开连接时拷的，整条写回会盖掉之后改的名字、写回已删的记录。
                ignoringErrors { machines.updateLastAddress(machine.id, address) }
            }
            return Session(transport, reader.decoder, outcome.hostName, welcome.keepAwake, welcome.findsCommands)
        } catch (error: Throwable) {
            transport.close()
            throw error
        }
    }

    private suspend fun readWelcome(reader: FrameReader): HostMsg.Welcome {
        while (true) {
            when (val message = reader.nextControl().message(HostMsg.serializer())) {
                is HostMsg.Welcome -> return message
                is HostMsg.Incompatible -> throw LinkFailure.Incompatible(message.reason)
                is HostMsg.Goodbye -> throw LinkFailure.Goodbye(message.reason)
                else -> {}
            }
        }
    }

    /**
     * 读到连接断开，返回断开的原因。
     *
     * 订阅者的事件队列不限长，读多少就攒多少：手机喂 VT 跟不上时输出全堆在内存里，画面越落越后，宿主那边却看不到积压，
     * 不会让会话改发 `Resync`。所以有订阅者积压过了 [backlogLimit] 时先不读，等它取走，积压留在 TCP 和宿主那边。
     *
     * 刷屏时宿主转来的输出帧很碎（一帧常常只有一两行），订阅者每取一帧都有固定开销；所以一次读到的帧里，同一通道上
     * 连着的输出（或快照）先拼成一帧再交出去。
     */
    private suspend fun readUntilClosed(transport: FrameTransport, decoder: FrameDecoder): LinkFailure {
        var goodbye: GoodbyeReason? = null
        try {
            while (true) {
                var held: Held? = null
                while (true) {
                    val frame = decoder.next() ?: break
                    val holding = held
                    if (holding != null && frame.kind == holding.kind && frame.channel == holding.channel) {
                        holding.append(frame.payload)
                        continue
                    }
                    holding?.let { handle(it.frame()) }
                    held = null
                    if (frame.kind == FrameKind.Output || frame.kind == FrameKind.Snapshot) {
                        held = Held(frame)
                    } else {
                        val message = handle(frame)
                        if (message is HostMsg.Goodbye) goodbye = message.reason
                    }
                }
                held?.let { handle(it.frame()) }
                // shortcut: 积压时每 5 毫秒看一次，不等订阅者通知；积压本来就要几十毫秒才消化得掉。
                while (subscribers.values.any { it.backlog.get() > backlogLimit }) delay(5.milliseconds)
                val chunk = transport.receive()
                if (chunk == null) {
                    decoder.finish()
                    break
                }
                decoder.append(chunk)
            }
        } catch (error: FrameError) {
            return LinkFailure.ProtocolViolation("malformed frame: ${error.message}")
        } catch (_: CancellationException) {
            return LinkFailure.HostClosed
        } catch (failure: LinkFailure) {
            return failure
        } catch (error: Exception) {
            return LinkFailure.Closed(error.message ?: error.toString())
        }
        return goodbye?.let { LinkFailure.Goodbye(it) } ?: LinkFailure.HostClosed
    }

    /** 拼着的一串同类型、同通道的帧。 */
    private class Held(private val first: Frame) {
        val kind: FrameKind get() = first.kind
        val channel: UInt get() = first.channel
        private var rest: ByteArrayOutputStream? = null

        fun append(payload: ByteArray) {
            val buffer = rest ?: ByteArrayOutputStream(first.payload.size + payload.size).also {
                it.write(first.payload)
                rest = it
            }
            buffer.write(payload)
        }

        fun frame(): Frame = rest?.let { Frame(first.kind, first.channel, it.toByteArray()) } ?: first
    }

    /** 把一帧交给上层；返回解出来的控制消息。 */
    private fun handle(frame: Frame): HostMsg? = when (frame.kind) {
        FrameKind.Control -> {
            val message = try {
                frame.message(HostMsg.serializer())
            } catch (_: Exception) {
                null
            }
            message?.also { broadcast(HostEvent.Message(it)) }
        }
        FrameKind.Output, FrameKind.Snapshot -> {
            broadcast(HostEvent.Frame(frame, generation))
            null
        }
        FrameKind.Input, FrameKind.Video, FrameKind.Audio -> null
    }

    private fun write(item: Outgoing) {
        val transport = transport ?: return
        val frame = when (item) {
            is Outgoing.Message -> {
                // 宿主对不认得的消息回不带编号的 `Error`，会被等着回话的页面当成自己的。
                if (item.message == ClientMsg.KeepAwake && !hostKeepsAwake) return
                if (item.message is ClientMsg.FindCommands && !hostFindsCommands) return
                try {
                    Frame.control(item.message)
                } catch (_: Exception) {
                    return
                }
            }
            is Outgoing.Input -> {
                if (item.generation != generation) return
                Frame(FrameKind.Input, item.channel, item.data)
            }
        }
        val bytes = try {
            frame.encoded()
        } catch (_: FrameError) {
            return
        }
        transport.send(bytes)
    }

    /** 一个订阅者的事件队列和转出去、还没取走的输出和快照字节数。 */
    private class Subscriber {
        val events = Channel<HostEvent>(Channel.UNLIMITED)
        val backlog = AtomicInteger(0)
    }

    private companion object {
        /** 一个订阅者转出去、还没取走的输出和快照最多这么多字节，再多就先不读。 */
        const val backlogLimit = 256 shl 10
    }
}

/** 记下失败也不要紧的写（比如记住上次的地址）；取消照常往外抛。 */
private suspend fun ignoringErrors(block: suspend () -> Unit) {
    try {
        block()
    } catch (cancelled: CancellationException) {
        throw cancelled
    } catch (_: Exception) {
    }
}

/** 等到 [mark] 那一刻；已经过了就马上返回。 */
private suspend fun delayUntil(mark: TimeMark) = delay(-mark.elapsedNow())

/** 同时跑 [blocks]，任何一个结束就取消其余的。 */
private suspend fun firstOf(vararg blocks: suspend () -> Unit) = coroutineScope {
    val jobs = blocks.map { block -> launch { block() } }
    select { jobs.forEach { job -> job.onJoin {} } }
    jobs.forEach { it.cancel() }
}
