package dev.runode.mobile.features.model

import dev.runode.mobile.connection.HostEvent
import dev.runode.mobile.connection.LinkFailure
import dev.runode.mobile.connection.LinkState
import dev.runode.mobile.connection.MemoryDeviceKeyStore
import dev.runode.mobile.connection.MemoryMachineStore
import dev.runode.mobile.connection.PushRelay
import dev.runode.mobile.connection.PushRelayError
import dev.runode.mobile.protocol.Agent
import dev.runode.mobile.protocol.AgentKind
import dev.runode.mobile.protocol.AgentState
import dev.runode.mobile.protocol.ApnsEnv
import dev.runode.mobile.protocol.ClientMsg
import dev.runode.mobile.protocol.GateRejection
import dev.runode.mobile.protocol.HostMsg
import dev.runode.mobile.protocol.PushService
import dev.runode.mobile.protocol.RelayRegister
import dev.runode.mobile.protocol.SessionId
import dev.runode.mobile.protocol.SessionInfo
import dev.runode.mobile.protocol.SessionLink
import dev.runode.mobile.protocol.SessionMeta
import java.time.Instant
import java.util.UUID
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlin.time.Duration
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.Duration.Companion.seconds
import kotlinx.coroutines.async
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.receiveAsFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runTest
import org.junit.Rule

/** 假的通知系统接口：权限由测试定，token 由测试推，记下要了几次权限和 token、删了哪些通知。 */
class FakePushNotifications : PushNotifications {
    var allowed = true
    var authorizationRequests = 0
    var remoteRegistrations = 0
    var shown: List<DeliveredNotification> = emptyList()
    val removed = mutableListOf<String>()
    private val tokens = Channel<String>(Channel.UNLIMITED)
    private val failures = Channel<String>(Channel.UNLIMITED)

    override suspend fun requestAuthorization(): Boolean {
        authorizationRequests += 1
        return allowed
    }

    override fun registerForRemoteNotifications() {
        remoteRegistrations += 1
    }

    override fun deviceTokens(): Flow<String> = tokens.receiveAsFlow()

    override fun registrationFailures(): Flow<String> = failures.receiveAsFlow()

    override suspend fun delivered(): List<DeliveredNotification> = shown

    override fun removeDelivered(ids: List<String>) {
        removed += ids
        shown = shown.filter { it.id !in ids }
    }

    fun give(token: String) {
        tokens.trySend(token)
    }

    fun fail(reason: String) {
        failures.trySend(reason)
    }
}

/** 假的中转服务：句柄是 `h-` 加 token，[failure] 不为空时换不到。 */
class FakePushRelay : PushRelay {
    val requests = mutableListOf<RelayRegister>()
    var failure: PushRelayError? = null

    override suspend fun handle(request: RelayRegister): String {
        requests += request
        failure?.let { throw it }
        return "h-${request.token}"
    }
}

/** 发出去的登记推送，按先后。 */
val FakeLink.registrations: List<ClientMsg.PushRegister> get() = sent.filterIsInstance<ClientMsg.PushRegister>()

/** 发出去的解除配对的请求编号，按先后。 */
val FakeLink.unpairs: List<UInt> get() = sent.filterIsInstance<ClientMsg.Unpair>().map { it.req }

val pushIdentity = PushIdentity(ApnsEnv.Production, "dev.runode.mobile", PushService.Fcm)
private const val firstToken = "80f00a"
private const val secondToken = "0102"

private val dev.runode.mobile.connection.MachineRecord.wireId: String get() = id.toString().uppercase()

class PushRegistrationTests {
    @get:Rule val main = MainDispatcherRule()

    private val system = FakePushNotifications()
    private val machine = machineRecord(name = "书房的 Mac")
    private val link = FakeLink()

    private fun registration(
        enabled: Boolean = true,
        identity: PushIdentity? = pushIdentity,
        relay: PushRelay? = null,
        timeout: Duration = 5.seconds,
    ) = PushRegistration(system, identity, relay, enabled, timeout)

    @Test fun registersOnceConnectedAndTheTokenIsKnown() = runTest {
        val push = registration()
        push.connected(machine, link)
        delay(20)
        // 还没拿到 token：先不发。
        assertTrue(link.registrations.isEmpty())
        system.give(firstToken)
        assertTrue(eventually { link.registrations.size == 1 })
        assertEquals(
            ClientMsg.PushRegister(
                req = 1u,
                token = "80f00a",
                env = ApnsEnv.Production,
                bundle = "dev.runode.mobile",
                machine = machine.wireId,
                machineName = "书房的 Mac",
                service = PushService.Fcm,
            ),
            link.registrations.first(),
        )
    }

    @Test fun aNewTokenReachesConnectedMachinesOnly() = runTest {
        val push = registration()
        val other = machineRecord(name = "homelab", fingerprintByte = 2)
        val otherLink = FakeLink()
        system.give(firstToken)
        assertTrue(eventually { push.token == "80f00a" })
        push.connected(machine, link)
        push.connected(other, otherLink)
        assertTrue(eventually { link.registrations.size == 1 && otherLink.registrations.size == 1 })
        push.disconnected(other.id)
        // 一样的 token 不重发。
        system.give(firstToken)
        system.give(secondToken)
        assertTrue(eventually { link.registrations.size == 2 })
        assertEquals("0102", link.registrations.last().token)
        delay(20)
        assertEquals(2, link.registrations.size)
        assertEquals(1, otherLink.registrations.size)
    }

    @Test fun turningAlertsOffUnregistersAndOnRegistersAgain() = runTest {
        val push = registration()
        system.give(firstToken)
        assertTrue(eventually { push.token != null })
        push.connected(machine, link)
        assertTrue(eventually { link.registrations.size == 1 })
        push.enabled = false
        assertTrue(eventually { link.registrations.size == 2 })
        assertNull(link.registrations.last().token)
        push.enabled = true
        assertTrue(eventually { link.registrations.size == 3 })
        assertEquals("80f00a", link.registrations.last().token)
    }

    /** 关着提醒时连上的电脑也注销，免得关之前没连着的电脑还在推；没拿到 token 也照样注销。 */
    @Test fun machinesConnectingWhileOffAreUnregistered() = runTest {
        val push = registration(enabled = false)
        push.connected(machine, link)
        assertTrue(eventually { link.registrations.size == 1 })
        assertNull(link.registrations.first().token)
        assertEquals(PushService.Fcm, link.registrations.first().service)
    }

    @Test fun repliesSetTheStatus() = runTest {
        val push = registration()
        system.give(firstToken)
        assertTrue(eventually { push.token != null })
        push.connected(machine, link)
        assertTrue(eventually { link.registrations.size == 1 })
        val req = link.registrations.last().req
        // 别的请求的回话不算。
        push.handle(HostMsg.Done(req + 100u), machine.id)
        assertNull(push.statuses[machine.id])
        push.handle(HostMsg.Done(req), machine.id)
        assertEquals(PushRegistration.Status.Registered, push.statuses[machine.id])
        // 再登记一次，电脑回带编号的错误。
        push.enabled = false
        assertTrue(eventually { link.registrations.size == 2 })
        val second = link.registrations.last().req
        push.handle(HostMsg.Error(second, null, "cannot update the paired device table"), machine.id)
        assertEquals(
            PushRegistration.Status.Failed(UiText.Raw("cannot update the paired device table")),
            push.statuses[machine.id],
        )
    }

    @Test fun disconnectingStopsWaitingForTheReply() = runTest {
        val push = registration(timeout = 50.milliseconds)
        system.give(firstToken)
        assertTrue(eventually { push.token != null })
        push.connected(machine, link)
        assertTrue(eventually { link.registrations.size == 1 })
        push.disconnected(machine.id)
        delay(120)
        assertNull(push.statuses[machine.id])
    }

    /** 注销着的时候 token 变了：不再登记，顶不掉注销，注销照样等到电脑回话。 */
    @Test fun aMachineBeingDeletedIsNotRegisteredAgain() = runTest {
        val push = registration()
        system.give(firstToken)
        assertTrue(eventually { push.token != null })
        push.connected(machine, link)
        assertTrue(eventually { link.registrations.size == 1 })
        val leaving = async { push.unregister(machine.id, 5.seconds) }
        assertTrue(eventually { link.registrations.size == 2 })
        system.give(secondToken)
        assertTrue(eventually { push.token == "0102" })
        delay(20)
        assertEquals(2, link.registrations.size)
        assertNull(link.registrations.last().token)
        push.handle(HostMsg.Done(link.registrations.last().req), machine.id)
        assertTrue(leaving.await())
        assertEquals(PushRegistration.Status.Registered, push.statuses[machine.id])
    }

    @Test fun renamingResendsWithTheNewName() = runTest {
        val push = registration()
        system.give(firstToken)
        assertTrue(eventually { push.token != null })
        push.connected(machine, link)
        assertTrue(eventually { link.registrations.size == 1 })
        push.renamed(machine)
        push.renamed(machine.copy(name = "客厅的 Mac"))
        assertTrue(eventually { link.registrations.size == 2 })
        assertEquals("客厅的 Mac", link.registrations.last().machineName)
    }

    /** 有中转服务时电脑只拿到为它换来的句柄；token 没变时不再换，变了再换。 */
    @Test fun withARelayMachinesGetHandlesInsteadOfTokens() = runTest {
        val relay = FakePushRelay()
        val push = registration(relay = relay)
        system.give(firstToken)
        assertTrue(eventually { push.token != null })
        push.connected(machine, link)
        assertTrue(eventually { link.registrations.size == 1 })
        assertEquals("h-80f00a", link.registrations.last().token)
        assertEquals(
            listOf(RelayRegister(env = ApnsEnv.Production, token = "80f00a", machine = machine.wireId, service = PushService.Fcm)),
            relay.requests,
        )
        push.renamed(machine.copy(name = "客厅的 Mac"))
        assertTrue(eventually { link.registrations.size == 2 })
        assertEquals("h-80f00a", link.registrations.last().token)
        assertEquals(1, relay.requests.size)
        system.give(secondToken)
        assertTrue(eventually { link.registrations.size == 3 })
        assertEquals("h-0102", link.registrations.last().token)
        assertEquals(2, relay.requests.size)
    }

    /** 断开再连上时重新换句柄：电脑不认句柄时会删掉登记、不告诉手机，交回旧的还是推不到。 */
    @Test fun reconnectingExchangesTheHandleAgain() = runTest {
        val relay = FakePushRelay()
        val push = registration(relay = relay)
        system.give(firstToken)
        assertTrue(eventually { push.token != null })
        push.connected(machine, link)
        assertTrue(eventually { link.registrations.size == 1 })
        push.disconnected(machine.id)
        val again = FakeLink()
        push.connected(machine, again)
        assertTrue(eventually { again.registrations.size == 1 })
        assertEquals(2, relay.requests.size)
    }

    /** 换不到句柄时不登记，原因和电脑回错一样记在设置页上。 */
    @Test fun relayFailuresAreShown() = runTest {
        val relay = FakePushRelay()
        relay.failure = PushRelayError(429, "TooManyRequests")
        val push = registration(relay = relay)
        system.give(firstToken)
        assertTrue(eventually { push.token != null })
        push.connected(machine, link)
        val failed = PushRegistration.Status.Failed(UiText.Raw("push relay: HTTP 429 TooManyRequests"))
        assertTrue(eventually { push.statuses[machine.id] == failed })
        assertTrue(link.registrations.isEmpty())
    }

    @Test fun withoutAnIdentityNothingIsSent() = runTest {
        val push = registration(identity = null)
        system.give(firstToken)
        assertTrue(eventually { push.token != null })
        push.connected(machine, link)
        delay(20)
        assertTrue(link.registrations.isEmpty())
    }

    /** 连上电脑时要通知权限，允许了才要 token。 */
    @Test fun connectingAsksForPermissionThenForAToken() = runTest {
        val push = registration()
        push.connected(machine, link)
        assertTrue(eventually { system.remoteRegistrations == 1 })
        assertEquals(1, system.authorizationRequests)
        assertTrue(push.systemAllowed)
    }

    /** 系统不允许发通知：不要 token，对电脑注销，拿到 token 也不登记；设置页据此提示。 */
    @Test fun deniedPermissionUnregisters() = runTest {
        system.allowed = false
        val push = registration()
        push.connected(machine, link)
        assertTrue(eventually { link.registrations.size == 1 })
        assertNull(link.registrations.first().token)
        assertFalse(push.systemAllowed)
        system.give(firstToken)
        assertTrue(eventually { push.token != null })
        delay(20)
        assertEquals(1, link.registrations.size)
        assertEquals(0, system.remoteRegistrations)
    }

    /** 登记过以后在系统设置里关了通知：再连上时注销，电脑不再推问题的内容。 */
    @Test fun permissionRevokedLaterUnregisters() = runTest {
        val push = registration()
        system.give(firstToken)
        assertTrue(eventually { push.token != null })
        push.connected(machine, link)
        assertTrue(eventually { link.registrations.size == 1 })
        assertEquals("80f00a", link.registrations.first().token)
        push.disconnected(machine.id)
        system.allowed = false
        push.connected(machine, link)
        assertTrue(eventually { link.registrations.size == 2 })
        assertNull(link.registrations.last().token)
    }

    /** 在系统设置里打开了通知，再连上时登记，token 没变也一样。 */
    @Test fun permissionGrantedLaterRegisters() = runTest {
        system.allowed = false
        val push = registration()
        system.give(firstToken)
        assertTrue(eventually { push.token != null })
        push.connected(machine, link)
        assertTrue(eventually { link.registrations.size == 1 })
        assertFalse(push.systemAllowed)
        push.disconnected(machine.id)
        system.allowed = true
        push.connected(machine, link)
        assertTrue(eventually { link.registrations.size == 2 })
        assertNull(link.registrations.first().token)
        assertEquals("80f00a", link.registrations.last().token)
        assertTrue(push.systemAllowed)
    }

    /** 要 token 失败了记下原因给设置页，之后拿到 token 就清掉。 */
    @Test fun tokenFailuresAreShownUntilATokenArrives() = runTest {
        val push = registration()
        system.fail("SERVICE_NOT_AVAILABLE")
        assertTrue(eventually { push.tokenError == UiText.Raw("SERVICE_NOT_AVAILABLE") })
        system.give(firstToken)
        assertTrue(eventually { push.tokenError == null })
    }

    /** 关着提醒时不要权限：连上的电脑只注销。 */
    @Test fun alertsOffDoNotAskForPermission() = runTest {
        val push = registration(enabled = false)
        push.connected(machine, link)
        assertTrue(eventually { link.registrations.size == 1 })
        assertEquals(0, system.authorizationRequests)
        push.enabled = true
        assertTrue(eventually { system.authorizationRequests == 1 })
    }

    /** 只有官方 App 经中转、FCM 推；别的 applicationId 不登记。 */
    @Test fun identityComesFromTheApplicationId() {
        assertEquals(pushIdentity, PushIdentity.of("dev.runode.mobile"))
        assertNull(PushIdentity.of("dev.runode.mobile.debug"))
        assertNull(PushIdentity.of("com.example.runode"))
    }
}

/** [AppModel] 把推送登记、清掉答完的通知接到各台电脑的连接上。 */
class AppPushTests {
    @get:Rule val main = MainDispatcherRule()

    private val machine = machineRecord()
    private val link = FakeLink()
    private val system = FakePushNotifications()
    private val preferences = DefaultsStore<AppPreferences>("preferences")
    private val store = MemoryMachineStore()
    private val keys = MemoryDeviceKeyStore()

    private suspend fun app(load: Boolean = true): AppModel {
        store.upsert(machine)
        keys.generate(machine.id)
        val dependencies = AppDependencies(
            store = store,
            keyStore = keys,
            pairing = FakePairing { machineRecord() },
            makeLink = { link },
            deviceName = "测试手机",
            preferences = preferences,
        )
        dependencies.notifications = system
        dependencies.pushIdentity = pushIdentity
        val app = AppModel(dependencies)
        if (load) app.machineList.load()
        return app
    }

    private fun info(id: SessionId, state: AgentState?) =
        SessionInfo(id, smallGrid, SessionMeta(title = "t", agent = state?.let { Agent(AgentKind("claude"), it) }))

    private fun notification(session: SessionId, machine: UUID = this.machine.id) =
        DeliveredNotification("$machine-$session", SessionLink(machine, session.rawValue))

    private fun connect(list: SessionListModel) {
        list.handle(HostEvent.State(LinkState.Connected("MacBook", null)))
        list.handle(HostEvent.Ready(1u))
    }

    @Test fun connectingRegistersAndTheSettingUnregisters() = runTest {
        val app = app()
        system.give("ab")
        assertTrue(eventually { app.push.token == "ab" })
        val list = assertNotNull(app.sessionList(machine.id))
        connect(list)
        assertTrue(eventually { link.registrations.size == 1 })
        assertEquals(machine.wireId, link.registrations.first().machine)
        // 回话经会话列表的事件转到登记那边。
        list.handle(HostEvent.Message(HostMsg.Done(link.registrations.first().req)))
        assertEquals(PushRegistration.Status.Registered, app.push.statuses[machine.id])
        app.settings.preferences = app.settings.preferences.copy(alertsBlockedAgents = false)
        assertTrue(eventually { link.registrations.size == 2 })
        assertNull(link.registrations.last().token)
        assertEquals(false, preferences.load()?.alertsBlockedAgents)
    }

    /** 删掉连着的电脑：先请电脑解除配对（推送登记一起删），电脑回了话才断开、删掉记录，不用另外注销推送。 */
    @Test fun deletingAConnectedMachineUnpairsFirst() = runTest {
        val app = app()
        system.give("ab")
        assertTrue(eventually { app.push.token == "ab" })
        val list = assertNotNull(app.sessionList(machine.id))
        connect(list)
        assertTrue(eventually { link.registrations.size == 1 })
        val deleting = launch { app.machineList.delete(machine.id) }
        assertTrue(eventually { link.unpairs.size == 1 })
        delay(20)
        // 还在等回话：没断开，记录也还在。
        assertEquals(0, link.stops)
        assertNotNull(app.machineList.machine(machine.id))
        list.handle(HostEvent.Message(HostMsg.Done(link.unpairs.first())))
        deleting.join()
        assertTrue(app.machineList.machines.isEmpty())
        assertTrue(store.all().isEmpty())
        assertTrue(eventually { link.stops == 1 })
        assertEquals(1, link.registrations.size)
        assertNull(app.push.statuses[machine.id])
    }

    /** 电脑不肯解除配对（宿主回错）时退回只注销推送，注销好了照样删掉，免得电脑以后还往这部手机推它的提醒。 */
    @Test fun aComputerThatCannotUnpairIsUnregistered() = runTest {
        val app = app()
        system.give("ab")
        assertTrue(eventually { app.push.token == "ab" })
        val list = assertNotNull(app.sessionList(machine.id))
        connect(list)
        assertTrue(eventually { link.registrations.size == 1 })
        val deleting = launch { app.machineList.delete(machine.id) }
        assertTrue(eventually { link.unpairs.size == 1 })
        list.handle(HostEvent.Message(HostMsg.Error(link.unpairs.first(), null, "unknown message")))
        assertTrue(eventually { link.registrations.size == 2 })
        assertNull(link.registrations.last().token)
        assertEquals(machine.wireId, link.registrations.last().machine)
        list.handle(HostEvent.Message(HostMsg.Done(link.registrations.last().req)))
        deleting.join()
        assertTrue(app.machineList.machines.isEmpty())
        assertTrue(eventually { link.stops == 1 })
    }

    /** 没连着的电脑删掉时什么都不发，也不等；记录和私钥留着，下次连上时请它解除配对、不登记推送，电脑回了话才真删。 */
    @Test fun aDisconnectedMachineIsUnpairedWhenItConnects() = runTest {
        val app = app()
        system.give("ab")
        assertTrue(eventually { app.push.token == "ab" })
        app.machineList.delete(machine.id)
        assertTrue(app.machineList.machines.isEmpty())
        assertTrue(link.sent.isEmpty())
        assertNotNull(keys.key(machine.id))
        assertFalse(app.showsNotification(SessionLink(machine.id, "1")))
        val list = assertNotNull(app.sessionList(machine.id))
        connect(list)
        assertTrue(eventually { link.unpairs.size == 1 })
        delay(20)
        assertTrue(link.registrations.isEmpty())
        list.handle(HostEvent.Message(HostMsg.Done(link.unpairs.first())))
        assertTrue(eventually { app.machineList.leaving.isEmpty() })
        assertTrue(store.all().isEmpty())
        assertNull(keys.key(machine.id))
    }

    /** 删掉没连着的电脑后，连上时解除配对和注销推送都回错：照样留着等下次；电脑撤销了这部手机：直接真删。 */
    @Test fun aLeavingMachineStaysUntilItIsReallyForgotten() = runTest {
        val app = app()
        system.give("ab")
        assertTrue(eventually { app.push.token == "ab" })
        app.machineList.delete(machine.id)
        val list = assertNotNull(app.sessionList(machine.id))
        connect(list)
        assertTrue(eventually { link.unpairs.size == 1 })
        list.handle(HostEvent.Message(HostMsg.Error(link.unpairs.first(), null, "unknown message")))
        assertTrue(eventually { link.registrations.size == 1 })
        list.handle(HostEvent.Message(HostMsg.Error(link.registrations.first().req, null, "cannot write the device table")))
        delay(20)
        assertEquals(listOf(machine.id), app.machineList.leaving.map { it.id })
        assertNotNull(keys.key(machine.id))
        list.handle(HostEvent.State(LinkState.Failed(LinkFailure.Rejected(GateRejection.UnknownDevice))))
        assertTrue(eventually { app.machineList.leaving.isEmpty() })
        assertTrue(store.all().isEmpty())
        assertNull(keys.key(machine.id))
    }

    /** 不认识的电脑的通知不出；App 在前台、终端页正显示着那个会话时也不出，在后台、看着别的会话时照出。 */
    @Test fun notificationsOfUnknownMachinesAndTheShownSessionAreNotShown() = runTest {
        val app = app()
        val link = SessionLink(machine.id, sessionA.rawValue)
        assertTrue(app.showsNotification(link))
        assertFalse(app.showsNotification(SessionLink(UUID.randomUUID(), "1")))
        app.path = listOf(Route.Machine(machine.id), Route.Terminal(machine.id, sessionA))
        assertFalse(app.showsNotification(link))
        assertTrue(app.showsNotification(SessionLink(machine.id, sessionB.rawValue)))
        app.setActive(false)
        assertTrue(app.showsNotification(link))
        app.setActive(true)
        app.path = emptyList()
    }

    /** 进后台断开时不再等回话（不然到时候会被当成不支持），回前台连上时再登记。 */
    @Test fun backgroundStopsWaitingAndForegroundRegistersAgain() = runTest {
        val app = app()
        system.give("ab")
        assertTrue(eventually { app.push.token == "ab" })
        val list = assertNotNull(app.sessionList(machine.id))
        connect(list)
        assertTrue(eventually { link.registrations.size == 1 })
        val req = link.registrations.first().req
        app.setActive(false)
        list.handle(HostEvent.Message(HostMsg.Done(req)))
        assertNull(app.push.statuses[machine.id])
        app.setActive(true)
        connect(list)
        assertTrue(eventually { link.registrations.size == 2 })
    }

    @Test fun answeredNotificationsAreClearedInTheForeground() = runTest {
        val app = app()
        val elsewhere = UUID.randomUUID()
        system.shown = listOf(
            notification(sessionA),
            notification(sessionB),
            notification(sessionC),
            notification(sessionA, machine = elsewhere),
        )
        val list = assertNotNull(app.sessionList(machine.id))
        connect(list)
        // A 已经不等了，B 还在等，C 没了；别的电脑上的不动。
        list.handle(HostEvent.Message(HostMsg.SessionList(listOf(info(sessionA, AgentState.Working), info(sessionB, AgentState.Blocked)))))
        assertTrue(eventually { system.removed.size == 2 })
        assertEquals(setOf(notification(sessionA).id, notification(sessionC).id), system.removed.toSet())
        assertEquals(listOf(notification(sessionB), notification(sessionA, machine = elsewhere)), system.shown)
        // 答完了也清掉。
        list.handle(HostEvent.Message(HostMsg.Meta(sessionB, info(sessionB, AgentState.Working).meta)))
        assertTrue(eventually { system.removed.size == 3 })
    }

    /** 重连上还没收到新列表时手上是断开前的列表，不能拿它判断；在后台时也不动。 */
    @Test fun staleListsAndTheBackgroundLeaveNotificationsAlone() = runTest {
        val app = app()
        val list = assertNotNull(app.sessionList(machine.id))
        connect(list)
        list.handle(HostEvent.Message(HostMsg.SessionList(listOf(info(sessionA, AgentState.Working)))))
        list.handle(HostEvent.State(LinkState.Waiting(LinkFailure.Timeout, Instant.now())))
        system.shown = listOf(notification(sessionA))
        connect(list)
        list.handle(HostEvent.Message(HostMsg.Meta(sessionA, info(sessionA, AgentState.Working).meta)))
        delay(20)
        assertTrue(system.removed.isEmpty())
        app.setActive(false)
        list.handle(HostEvent.Message(HostMsg.SessionList(listOf(info(sessionA, AgentState.Working)))))
        delay(20)
        assertTrue(system.removed.isEmpty())
        app.setActive(true)
        connect(list)
        list.handle(HostEvent.Message(HostMsg.SessionList(listOf(info(sessionA, AgentState.Working)))))
        assertTrue(eventually { system.removed == listOf(notification(sessionA).id) })
    }
}
