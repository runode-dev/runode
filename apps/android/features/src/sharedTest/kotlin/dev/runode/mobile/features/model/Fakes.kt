package dev.runode.mobile.features.model

import dev.runode.mobile.connection.DeviceKeyStore
import dev.runode.mobile.connection.HostEvent
import dev.runode.mobile.connection.HostLink
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.connection.MemoryDeviceKeyStore
import dev.runode.mobile.connection.Pairing
import dev.runode.mobile.protocol.CertificateFingerprint
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.GridSize
import dev.runode.mobile.protocol.PairingInvitation
import dev.runode.mobile.protocol.SessionId
import dev.runode.mobile.protocol.TermSettings
import dev.runode.mobile.terminal.TerminalDisplay
import dev.runode.mobile.terminal.VTerminal
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.SendChannel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.buffer
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.withContext

/** 假的连接：记下发出去的东西，事件由测试推（[emit]）。 */
class FakeLink : HostLink {
    class Input(val data: ByteArray, val channel: UInt, val generation: ULong)

    private val lock = Any()
    private val sentList = mutableListOf<ClientMsg>()
    private val inputList = mutableListOf<Input>()
    private val subscribers = mutableListOf<SendChannel<HostEvent>>()
    private val deviceNameList = mutableListOf<String>()
    private val focusedList = mutableListOf<Boolean>()
    private var request = 0u
    private var keepAwakeCount = 0
    private var startCount = 0
    private var stopCount = 0
    private var reconnectCount = 0
    private var subscribed = 0

    override val state: StateFlow<LinkState> = MutableStateFlow(LinkState.Idle)

    val sent: List<ClientMsg> get() = synchronized(lock) { sentList.toList() }

    /** 发过几次 `KeepAwake`；它们按时发，不记进 [sent]。 */
    val keepAwakes: Int get() = synchronized(lock) { keepAwakeCount }
    val inputs: List<Input> get() = synchronized(lock) { inputList.toList() }
    val starts: Int get() = synchronized(lock) { startCount }
    val stops: Int get() = synchronized(lock) { stopCount }
    val reconnects: Int get() = synchronized(lock) { reconnectCount }

    /** 一共订阅过几次（停掉的也算）。 */
    val subscriberCount: Int get() = synchronized(lock) { subscribed }
    val deviceNames: List<String> get() = synchronized(lock) { deviceNameList.toList() }

    /** 每次 `setFocused` 设的值。 */
    val focused: List<Boolean> get() = synchronized(lock) { focusedList.toList() }

    fun clearSent() {
        synchronized(lock) { sentList.clear() }
    }

    fun emit(event: HostEvent) {
        for (subscriber in synchronized(lock) { subscribers.toList() }) subscriber.trySend(event)
    }

    override fun events(): Flow<HostEvent> = callbackFlow {
        synchronized(lock) {
            subscribers += channel
            subscribed += 1
        }
        awaitClose { synchronized(lock) { subscribers -= channel } }
    }.buffer(Channel.UNLIMITED)

    override fun send(message: ClientMsg) {
        synchronized(lock) { if (message == ClientMsg.KeepAwake) keepAwakeCount += 1 else sentList += message }
    }

    override fun sendInput(data: ByteArray, channel: UInt, generation: ULong) {
        synchronized(lock) { inputList += Input(data, channel, generation) }
    }

    override suspend fun start() {
        synchronized(lock) { startCount += 1 }
    }

    override suspend fun stop() {
        synchronized(lock) { stopCount += 1 }
    }

    override suspend fun reconnectNow() {
        synchronized(lock) { reconnectCount += 1 }
    }

    override fun setFocused(focused: Boolean) {
        synchronized(lock) { focusedList += focused }
    }

    override suspend fun setDeviceName(name: String) {
        synchronized(lock) { deviceNameList += name }
    }

    override fun nextRequestId(): UInt = synchronized(lock) {
        request += 1u
        request
    }
}

/** 记下视图模型让它做的事的假显示。 */
class FakeDisplay : TerminalDisplay {
    var resets = 0
    var changes = 0
    var bells = 0

    /** 每次尺寸方式变了时报的「是不是适配手机」。 */
    val sizeModes = mutableListOf<Boolean>()
    var keyboardRequests = 0
    var settings: TermSettings? = null
    var terminal: VTerminal? = null

    override fun terminalDidReset(terminal: VTerminal?, settings: TermSettings) {
        resets += 1
        this.terminal = terminal
        this.settings = settings
    }

    override fun terminalContentDidChange() {
        changes += 1
    }

    override fun terminalSettingsDidChange(settings: TermSettings) {
        this.settings = settings
    }

    override fun terminalDidRingBell() {
        bells += 1
    }

    override fun terminalSizeModeDidChange(fitsPhone: Boolean) {
        sizeModes += fitsPhone
    }

    override fun terminalShowKeyboard() {
        keyboardRequests += 1
    }
}

/** 假的配对：按预先给的结果回。 */
class FakePairing(private val result: (PairingInvitation) -> MachineRecord) : Pairing {
    override suspend fun pair(invitation: PairingInvitation, deviceName: String): MachineRecord = result(invitation)
}

/**
 * 等 [gate] 里来一个值才配好的假配对；被撤掉时照样配好，看调用方还存不存。像真的配对一样把私钥存进 [keys]，
 * `discard` 时删掉。
 */
class GatedPairing(
    private val gate: Channel<Unit>,
    val keys: DeviceKeyStore = MemoryDeviceKeyStore(),
    /** 配好的电脑，每次都是它。 */
    val machine: MachineRecord = machineRecord(),
) : Pairing {
    override suspend fun pair(invitation: PairingInvitation, deviceName: String): MachineRecord {
        withContext(NonCancellable) { gate.receive() }
        keys.generate(machine.id)
        return machine
    }

    override fun discard(machine: MachineRecord) {
        keys.deleteKey(machine.id)
    }
}

val sessionA = SessionId("0123456789abcdef0011223344556677")!!
val sessionB = SessionId("ffffffffffffffff0000000000000000")!!
val sessionC = SessionId("cccccccccccccccc0000000000000000")!!
val smallGrid = GridSize(20u, 4u, 8u, 16u)

fun machineRecord(name: String = "MacBook", fingerprintByte: Byte = 1): MachineRecord = MachineRecord(
    name = name,
    hostName = "Ethan 的 MacBook Pro",
    fingerprint = CertificateFingerprint(ByteArray(32) { fingerprintByte })!!,
    port = 7866u,
    addresses = listOf("192.168.1.20"),
    lastAddress = null,
    deviceId = "00112233445566778899aabbccddeeff",
)

/** 等到条件成立，最多等两百轮。视图模型在自己的协程里消费事件，测试据此等它处理完。 */
suspend fun eventually(condition: () -> Boolean): Boolean {
    repeat(200) {
        if (condition()) return true
        delay(5)
    }
    return condition()
}
