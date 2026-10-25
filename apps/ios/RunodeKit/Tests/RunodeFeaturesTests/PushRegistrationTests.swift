import Foundation
import RunodeConnection
import RunodeProtocol
import Testing

@testable import RunodeFeatures

/// 假的通知系统接口：权限由测试定，token 由测试推，记下要了几次权限和 token、删了哪些通知。
@MainActor
final class FakePushNotifications: PushNotifications {
    var allowed = true
    var authorizationRequests = 0
    var remoteRegistrations = 0
    var shown: [DeliveredNotification] = []
    var removed: [String] = []
    private let tokens: AsyncStream<Data>
    private let tokenSink: AsyncStream<Data>.Continuation
    private let failures: AsyncStream<String>
    private let failureSink: AsyncStream<String>.Continuation

    init() {
        (tokens, tokenSink) = AsyncStream.makeStream(of: Data.self)
        (failures, failureSink) = AsyncStream.makeStream(of: String.self)
    }

    func requestAuthorization() async -> Bool {
        authorizationRequests += 1
        return allowed
    }

    func registerForRemoteNotifications() { remoteRegistrations += 1 }
    func deviceTokens() -> AsyncStream<Data> { tokens }
    func registrationFailures() -> AsyncStream<String> { failures }
    func delivered() async -> [DeliveredNotification] { shown }

    func removeDelivered(_ ids: [String]) {
        removed += ids
        shown.removeAll { ids.contains($0.id) }
    }

    func give(token: Data) { tokenSink.yield(token) }
    func fail(_ reason: String) { failureSink.yield(reason) }
}

/// 一次登记推送的消息里要核对的几项。
struct SentRegistration: Equatable {
    var req: UInt32
    var token: String?
    var env: ApnsEnv
    var bundle: String
    var machine: String
    var machineName: String
}

extension FakeLink {
    /// 发出去的登记推送，按先后。
    var registrations: [SentRegistration] {
        sent.compactMap {
            guard case let .pushRegister(req, token, env, bundle, machine, machineName) = $0 else { return nil }
            return SentRegistration(
                req: req, token: token, env: env, bundle: bundle, machine: machine, machineName: machineName)
        }
    }
}

/// 假的中转服务：句柄是 `h-` 加 device token，`failure` 不为空时换不到。
actor FakePushRelay: PushRelay {
    var requests: [RelayRegister] = []
    var failure: PushRelayError?

    func fail(with error: PushRelayError?) { failure = error }

    func handle(for request: RelayRegister) async throws -> String {
        requests.append(request)
        if let failure { throw failure }
        return "h-\(request.token)"
    }
}

let pushIdentity = PushIdentity(environment: .development, bundle: "dev.runode.mobile")
private let firstToken = Data([0x80, 0xf0, 0x0a])
private let secondToken = Data([0x01, 0x02])

@MainActor
@Suite struct PushRegistrationTests {
    let system = FakePushNotifications()
    let machine = machineRecord(name: "书房的 Mac")
    let link = FakeLink()

    func registration(
        enabled: Bool = true, identity: PushIdentity? = pushIdentity, relay: (any PushRelay)? = nil,
        timeout: Duration = .seconds(5)
    ) -> PushRegistration {
        PushRegistration(system: system, identity: identity, relay: relay, enabled: enabled, replyTimeout: timeout)
    }

    @Test func registersOnceConnectedAndTheTokenIsKnown() async {
        let push = registration()
        push.connected(machine, link: link)
        try? await Task.sleep(for: .milliseconds(20))
        // 还没拿到 token：先不发。
        #expect(link.registrations.isEmpty)
        system.give(token: firstToken)
        #expect(await eventually { link.registrations.count == 1 })
        #expect(
            link.registrations.first
                == SentRegistration(
                    req: 1, token: "80f00a", env: .development, bundle: "dev.runode.mobile",
                    machine: machine.id.uuidString, machineName: "书房的 Mac"))
    }

    @Test func aNewTokenReachesConnectedMachinesOnly() async {
        let push = registration()
        let other = machineRecord(name: "homelab", fingerprintByte: 2)
        let otherLink = FakeLink()
        system.give(token: firstToken)
        #expect(await eventually { push.token == "80f00a" })
        push.connected(machine, link: link)
        push.connected(other, link: otherLink)
        #expect(await eventually { link.registrations.count == 1 && otherLink.registrations.count == 1 })
        push.disconnected(other.id)
        // 一样的 token 不重发。
        system.give(token: firstToken)
        system.give(token: secondToken)
        #expect(await eventually { link.registrations.count == 2 })
        #expect(link.registrations.last?.token == "0102")
        try? await Task.sleep(for: .milliseconds(20))
        #expect(link.registrations.count == 2)
        #expect(otherLink.registrations.count == 1)
    }

    @Test func turningAlertsOffUnregistersAndOnRegistersAgain() async {
        let push = registration()
        system.give(token: firstToken)
        #expect(await eventually { push.token != nil })
        push.connected(machine, link: link)
        #expect(await eventually { link.registrations.count == 1 })
        push.enabled = false
        #expect(await eventually { link.registrations.count == 2 })
        #expect(link.registrations.last?.token == nil)
        push.enabled = true
        #expect(await eventually { link.registrations.count == 3 })
        #expect(link.registrations.last?.token == "80f00a")
    }

    /// 关着提醒时连上的电脑也注销，免得关之前没连着的电脑还在推；没拿到 token 也照样注销。
    @Test func machinesConnectingWhileOffAreUnregistered() async {
        let push = registration(enabled: false)
        push.connected(machine, link: link)
        #expect(await eventually { link.registrations.count == 1 })
        #expect(link.registrations.first?.token == nil)
        #expect(link.registrations.first?.env == .development)
    }

    @Test func repliesSetTheStatus() async throws {
        let push = registration()
        system.give(token: firstToken)
        #expect(await eventually { push.token != nil })
        push.connected(machine, link: link)
        #expect(await eventually { link.registrations.count == 1 })
        let req = try #require(link.registrations.last?.req)
        // 别的请求的回话不算。
        push.handle(.done(req: req + 100), from: machine.id)
        #expect(push.statuses[machine.id] == nil)
        push.handle(.done(req: req), from: machine.id)
        #expect(push.statuses[machine.id] == .registered)
        // 再登记一次，电脑回带编号的错误。
        push.enabled = false
        #expect(await eventually { link.registrations.count == 2 })
        let second = try #require(link.registrations.last?.req)
        push.handle(.error(req: second, id: nil, message: "cannot update the paired device table"), from: machine.id)
        #expect(push.statuses[machine.id] == .failed("cannot update the paired device table"))
    }

    @Test func disconnectingStopsWaitingForTheReply() async {
        let push = registration(timeout: .milliseconds(50))
        system.give(token: firstToken)
        #expect(await eventually { push.token != nil })
        push.connected(machine, link: link)
        #expect(await eventually { link.registrations.count == 1 })
        push.disconnected(machine.id)
        try? await Task.sleep(for: .milliseconds(120))
        #expect(push.statuses[machine.id] == nil)
    }

    /// 注销着的时候 token 变了：不再登记，顶不掉注销，注销照样等到电脑回话。
    @Test func aMachineBeingDeletedIsNotRegisteredAgain() async throws {
        let push = registration()
        system.give(token: firstToken)
        #expect(await eventually { push.token != nil })
        push.connected(machine, link: link)
        #expect(await eventually { link.registrations.count == 1 })
        let leaving = Task { await push.unregister(machine.id, timeout: .seconds(5)) }
        #expect(await eventually { link.registrations.count == 2 })
        system.give(token: secondToken)
        #expect(await eventually { push.token == "0102" })
        try await Task.sleep(for: .milliseconds(20))
        #expect(link.registrations.count == 2)
        #expect(link.registrations.last?.token == nil)
        let req = try #require(link.registrations.last?.req)
        push.handle(.done(req: req), from: machine.id)
        #expect(await leaving.value)
        #expect(push.statuses[machine.id] == .registered)
    }

    @Test func renamingResendsWithTheNewName() async {
        let push = registration()
        system.give(token: firstToken)
        #expect(await eventually { push.token != nil })
        push.connected(machine, link: link)
        #expect(await eventually { link.registrations.count == 1 })
        push.renamed(machine)
        var renamed = machine
        renamed.name = "客厅的 Mac"
        push.renamed(renamed)
        #expect(await eventually { link.registrations.count == 2 })
        #expect(link.registrations.last?.machineName == "客厅的 Mac")
    }

    /// 有中转服务时电脑只拿到为它换来的句柄；token 没变时不再换，变了再换。
    @Test func withARelayMachinesGetHandlesInsteadOfTokens() async {
        let relay = FakePushRelay()
        let push = registration(relay: relay)
        system.give(token: firstToken)
        #expect(await eventually { push.token != nil })
        push.connected(machine, link: link)
        #expect(await eventually { link.registrations.count == 1 })
        #expect(link.registrations.last?.token == "h-80f00a")
        #expect(
            await relay.requests
                == [RelayRegister(env: .development, token: "80f00a", machine: machine.id.uuidString)])
        var renamed = machine
        renamed.name = "客厅的 Mac"
        push.renamed(renamed)
        #expect(await eventually { link.registrations.count == 2 })
        #expect(link.registrations.last?.token == "h-80f00a")
        #expect(await relay.requests.count == 1)
        system.give(token: secondToken)
        #expect(await eventually { link.registrations.count == 3 })
        #expect(link.registrations.last?.token == "h-0102")
        #expect(await relay.requests.count == 2)
    }

    /// 断开再连上时重新换句柄：电脑不认句柄时会删掉登记、不告诉手机，交回旧的还是推不到。
    @Test func reconnectingExchangesTheHandleAgain() async {
        let relay = FakePushRelay()
        let push = registration(relay: relay)
        system.give(token: firstToken)
        #expect(await eventually { push.token != nil })
        push.connected(machine, link: link)
        #expect(await eventually { link.registrations.count == 1 })
        push.disconnected(machine.id)
        let again = FakeLink()
        push.connected(machine, link: again)
        #expect(await eventually { again.registrations.count == 1 })
        #expect(await relay.requests.count == 2)
    }

    /// 换不到句柄时不登记，原因和电脑回错一样记在设置页上。
    @Test func relayFailuresAreShown() async {
        let relay = FakePushRelay()
        await relay.fail(with: PushRelayError(status: 429, reason: "TooManyRequests"))
        let push = registration(relay: relay)
        system.give(token: firstToken)
        #expect(await eventually { push.token != nil })
        push.connected(machine, link: link)
        #expect(await eventually { push.statuses[machine.id] == .failed("push relay: HTTP 429 TooManyRequests") })
        #expect(link.registrations.isEmpty)
    }

    @Test func withoutAnIdentityNothingIsSent() async {
        let push = registration(identity: nil)
        system.give(token: firstToken)
        #expect(await eventually { push.token != nil })
        push.connected(machine, link: link)
        try? await Task.sleep(for: .milliseconds(20))
        #expect(link.registrations.isEmpty)
    }

    /// 连上电脑时要通知权限，允许了才向 APNs 要 token。
    @Test func connectingAsksForPermissionThenForAToken() async {
        let push = registration()
        push.connected(machine, link: link)
        #expect(await eventually { system.remoteRegistrations == 1 })
        #expect(system.authorizationRequests == 1)
        #expect(push.systemAllowed)
    }

    /// 系统不允许发通知：不要 token，对电脑注销，拿到 token 也不登记；设置页据此提示。
    @Test func deniedPermissionUnregisters() async {
        system.allowed = false
        let push = registration()
        push.connected(machine, link: link)
        #expect(await eventually { link.registrations.count == 1 })
        #expect(link.registrations.first?.token == nil)
        #expect(!push.systemAllowed)
        system.give(token: firstToken)
        #expect(await eventually { push.token != nil })
        try? await Task.sleep(for: .milliseconds(20))
        #expect(link.registrations.count == 1)
        #expect(system.remoteRegistrations == 0)
    }

    /// 登记过以后在系统设置里关了通知：再连上时注销，电脑不再把问题推给 APNs。
    @Test func permissionRevokedLaterUnregisters() async {
        let push = registration()
        system.give(token: firstToken)
        #expect(await eventually { push.token != nil })
        push.connected(machine, link: link)
        #expect(await eventually { link.registrations.count == 1 })
        #expect(link.registrations.first?.token == "80f00a")
        push.disconnected(machine.id)
        system.allowed = false
        push.connected(machine, link: link)
        #expect(await eventually { link.registrations.count == 2 })
        #expect(link.registrations.last?.token == nil)
    }

    /// 在系统设置里打开了通知，再连上时登记，token 没变也一样。
    @Test func permissionGrantedLaterRegisters() async {
        system.allowed = false
        let push = registration()
        system.give(token: firstToken)
        #expect(await eventually { push.token != nil })
        push.connected(machine, link: link)
        #expect(await eventually { link.registrations.count == 1 })
        #expect(!push.systemAllowed)
        push.disconnected(machine.id)
        system.allowed = true
        push.connected(machine, link: link)
        #expect(await eventually { link.registrations.count == 2 })
        #expect(link.registrations.first?.token == nil)
        #expect(link.registrations.last?.token == "80f00a")
        #expect(push.systemAllowed)
    }

    /// 要 device token 失败了记下原因给设置页，之后拿到 token 就清掉。
    @Test func tokenFailuresAreShownUntilATokenArrives() async {
        let push = registration()
        system.fail("no valid aps-environment entitlement")
        #expect(await eventually { push.tokenError == "no valid aps-environment entitlement" })
        system.give(token: firstToken)
        #expect(await eventually { push.tokenError == nil })
    }

    /// 关着提醒时不要权限：连上的电脑只注销。
    @Test func alertsOffDoNotAskForPermission() async {
        let push = registration(enabled: false)
        push.connected(machine, link: link)
        #expect(await eventually { link.registrations.count == 1 })
        #expect(system.authorizationRequests == 0)
        push.enabled = true
        #expect(await eventually { system.authorizationRequests == 1 })
    }

    @Test func identityComesFromTheInfoPlist() {
        #expect(
            PushIdentity(info: [PushIdentity.environmentKey: "production"], bundle: "dev.runode.mobile")
                == PushIdentity(environment: .production, bundle: "dev.runode.mobile"))
        #expect(
            PushIdentity(info: [PushIdentity.environmentKey: "development"], bundle: "x")?.environment
                == .development)
        // build setting 没展开、没写、写了不认识的，或者没有 bundle id：不登记。
        #expect(PushIdentity(info: [PushIdentity.environmentKey: "$(RUNODE_APS_ENVIRONMENT)"], bundle: "x") == nil)
        #expect(PushIdentity(info: [:], bundle: "x") == nil)
        #expect(PushIdentity(info: [PushIdentity.environmentKey: "production"], bundle: nil) == nil)
    }
}

/// `AppModel` 把推送登记、清掉答完的通知和打开会话接到各台电脑的连接上。
@MainActor
@Suite struct AppPushTests {
    let machine = machineRecord()
    let link = FakeLink()
    let system = FakePushNotifications()
    let preferences = DefaultsStore<AppPreferences>("preferences", defaults: .temporary())
    let store = MemoryMachineStore()
    let keys = MemoryDeviceKeyStore()

    func app(load: Bool = true) async -> AppModel {
        await store.upsert(machine)
        try? keys.save(StoredDeviceKey(kind: .software, data: Data([1])), for: machine.id)
        let link = self.link
        var dependencies = AppDependencies(
            store: store, keyStore: keys, pairing: FakePairing { _ in machineRecord() },
            makeLink: { _ in link }, deviceName: "测试 iPhone", preferences: preferences)
        dependencies.notifications = system
        dependencies.pushIdentity = pushIdentity
        let app = AppModel(dependencies: dependencies)
        if load { await app.machineList.load() }
        return app
    }

    func info(_ id: SessionId, _ state: AgentState?) -> SessionInfo {
        SessionInfo(
            id: id, size: smallGrid,
            meta: SessionMeta(title: "t", agent: state.map { Agent(kind: AgentKind("claude"), state: $0) }))
    }

    func notification(_ session: SessionId, machine: UUID? = nil) -> DeliveredNotification {
        let machine = machine ?? self.machine.id
        return DeliveredNotification(
            id: "\(machine)-\(session)", link: SessionLink(machine: machine, session: session.rawValue))
    }

    func connect(_ list: SessionListModel) {
        list.handle(.state(.connected(hostName: "MacBook", address: nil)))
        list.handle(.ready(generation: 1))
    }

    @Test func connectingRegistersAndTheSettingUnregisters() async throws {
        let app = await app()
        system.give(token: Data([0xab]))
        #expect(await eventually { app.push.token == "ab" })
        let list = try #require(app.sessionList(for: machine.id))
        connect(list)
        #expect(await eventually { link.registrations.count == 1 })
        #expect(link.registrations.first?.machine == machine.id.uuidString)
        // 回话经会话列表的事件转到登记那边。
        let req = try #require(link.registrations.first?.req)
        list.handle(.message(.done(req: req)))
        #expect(app.push.statuses[machine.id] == .registered)
        app.settings.preferences.alertsBlockedAgents = false
        #expect(await eventually { link.registrations.count == 2 })
        #expect(link.registrations.last?.token == nil)
        #expect(preferences.load()?.alertsBlockedAgents == false)
    }

    /// 删掉连着的电脑：先注销推送，电脑回了话才断开、删掉记录，免得电脑以后还往这部手机推它的提醒。
    @Test func deletingAConnectedMachineUnregistersFirst() async throws {
        let app = await app()
        system.give(token: Data([0xab]))
        #expect(await eventually { app.push.token == "ab" })
        let list = try #require(app.sessionList(for: machine.id))
        connect(list)
        #expect(await eventually { link.registrations.count == 1 })
        let deleting = Task { await app.machineList.delete(machine.id) }
        #expect(await eventually { link.registrations.count == 2 })
        #expect(link.registrations.last?.token == nil)
        #expect(link.registrations.last?.machine == machine.id.uuidString)
        try await Task.sleep(for: .milliseconds(20))
        // 还在等回话：没断开，记录也还在。
        #expect(link.stops == 0)
        #expect(app.machineList.machine(machine.id) != nil)
        let req = try #require(link.registrations.last?.req)
        list.handle(.message(.done(req: req)))
        await deleting.value
        #expect(app.machineList.machines.isEmpty)
        #expect(await eventually { link.stops == 1 })
        #expect(app.push.statuses[machine.id] == nil)
    }

    /// 没连着的电脑删掉时不发注销，也不等；记录和私钥留着，下次连上时只注销、不登记，电脑回了话才真删。
    @Test func aDisconnectedMachineIsUnregisteredWhenItConnects() async throws {
        let app = await app()
        system.give(token: Data([0xab]))
        #expect(await eventually { app.push.token == "ab" })
        await app.machineList.delete(machine.id)
        #expect(app.machineList.machines.isEmpty)
        #expect(link.registrations.isEmpty)
        #expect(try keys.key(for: machine.id) != nil)
        #expect(!app.showsNotification(for: SessionLink(machine: machine.id, session: "1")))
        let list = try #require(app.sessionList(for: machine.id))
        connect(list)
        #expect(await eventually { link.registrations.count == 1 })
        #expect(link.registrations.first?.token == nil)
        try await Task.sleep(for: .milliseconds(20))
        #expect(link.registrations.count == 1)
        let req = try #require(link.registrations.first?.req)
        list.handle(.message(.done(req: req)))
        #expect(await eventually { app.machineList.leaving.isEmpty })
        #expect(await store.all().isEmpty)
        #expect(try keys.key(for: machine.id) == nil)
    }

    /// 删掉没连着的电脑后，连上时电脑回错（登记还在）：照样留着等下次；电脑撤销了这部手机：直接真删。
    @Test func aLeavingMachineStaysUntilItIsReallyUnregistered() async throws {
        let app = await app()
        system.give(token: Data([0xab]))
        #expect(await eventually { app.push.token == "ab" })
        await app.machineList.delete(machine.id)
        let list = try #require(app.sessionList(for: machine.id))
        connect(list)
        #expect(await eventually { link.registrations.count == 1 })
        let req = try #require(link.registrations.first?.req)
        list.handle(.message(.error(req: req, id: nil, message: "cannot write the device table")))
        try await Task.sleep(for: .milliseconds(20))
        #expect(app.machineList.leaving.map(\.id) == [machine.id])
        #expect(try keys.key(for: machine.id) != nil)
        list.handle(.state(.failed(.rejected(.unknownDevice))))
        #expect(await eventually { app.machineList.leaving.isEmpty })
        #expect(await store.all().isEmpty)
        #expect(try keys.key(for: machine.id) == nil)
    }

    /// 前台时不认识的电脑的通知不弹，认识的照弹。
    @Test func notificationsOfUnknownMachinesAreNotShown() async throws {
        let app = await app()
        #expect(app.showsNotification(for: SessionLink(machine: machine.id, session: "1")))
        #expect(!app.showsNotification(for: SessionLink(machine: UUID(), session: "1")))
    }

    /// 进后台断开时不再等回话（不然到时候会被当成不支持），回前台连上时再登记。
    @Test func backgroundStopsWaitingAndForegroundRegistersAgain() async throws {
        let app = await app()
        system.give(token: Data([0xab]))
        #expect(await eventually { app.push.token == "ab" })
        let list = try #require(app.sessionList(for: machine.id))
        connect(list)
        #expect(await eventually { link.registrations.count == 1 })
        let req = try #require(link.registrations.first?.req)
        app.setActive(false)
        list.handle(.message(.done(req: req)))
        #expect(app.push.statuses[machine.id] == nil)
        app.setActive(true)
        connect(list)
        #expect(await eventually { link.registrations.count == 2 })
    }

    @Test func answeredNotificationsAreClearedInTheForeground() async throws {
        let app = await app()
        let elsewhere = UUID()
        system.shown = [
            notification(sessionA), notification(sessionB), notification(sessionC),
            notification(sessionA, machine: elsewhere),
        ]
        let list = try #require(app.sessionList(for: machine.id))
        connect(list)
        // A 已经不等了，B 还在等，C 没了；别的电脑上的不动。
        list.handle(.message(.sessionList([info(sessionA, .working), info(sessionB, .blocked)])))
        #expect(await eventually { system.removed.count == 2 })
        #expect(Set(system.removed) == [notification(sessionA).id, notification(sessionC).id])
        #expect(system.shown == [notification(sessionB), notification(sessionA, machine: elsewhere)])
        // 答完了也清掉。
        list.handle(.message(.meta(id: sessionB, meta: info(sessionB, .working).meta)))
        #expect(await eventually { system.removed.count == 3 })
    }

    /// 重连上还没收到新列表时手上是断开前的列表，不能拿它判断；在后台时也不动。
    @Test func staleListsAndTheBackgroundLeaveNotificationsAlone() async throws {
        let app = await app()
        let list = try #require(app.sessionList(for: machine.id))
        connect(list)
        list.handle(.message(.sessionList([info(sessionA, .working)])))
        list.handle(.state(.waiting(reason: "断了", retryAt: .now)))
        system.shown = [notification(sessionA)]
        connect(list)
        list.handle(.message(.meta(id: sessionA, meta: info(sessionA, .working).meta)))
        try await Task.sleep(for: .milliseconds(20))
        #expect(system.removed.isEmpty)
        app.setActive(false)
        list.handle(.message(.sessionList([info(sessionA, .working)])))
        try await Task.sleep(for: .milliseconds(20))
        #expect(system.removed.isEmpty)
        app.setActive(true)
        connect(list)
        list.handle(.message(.sessionList([info(sessionA, .working)])))
        #expect(await eventually { system.removed == [notification(sessionA).id] })
    }

    /// 点开通知：打开那个会话的终端页，设置页开着就关掉。
    @Test func openingANotificationOpensTheTerminal() async throws {
        let app = await app()
        app.showingSettings = true
        app.open(SessionLink(machine: machine.id, session: sessionB.rawValue))
        #expect(app.path == [.machine(machine.id), .terminal(machine: machine.id, session: sessionB)])
        #expect(!app.showingSettings)
    }

    @Test func sessionLinksOpenTheTerminal() async throws {
        let app = await app()
        let url = try #require(SessionLink(machine: machine.id, session: sessionA.rawValue).url)
        app.showingSettings = true
        app.open(url: url)
        #expect(app.path == [.machine(machine.id), .terminal(machine: machine.id, session: sessionA)])
        #expect(!app.showingSettings)
    }

    /// 冷启动时链接先到、电脑列表后读进来：读进来以后再打开。
    @Test func sessionLinksWaitForTheMachineList() async throws {
        let app = await app(load: false)
        app.open(url: try #require(SessionLink(machine: machine.id, session: sessionB.rawValue).url))
        #expect(app.path.isEmpty)
        await app.machineList.load()
        #expect(app.path == [.machine(machine.id), .terminal(machine: machine.id, session: sessionB)])
    }

    @Test(arguments: [
        // 不认识的电脑。
        "runode://session?machine=6F9619FF-8B86-D011-B42D-00C04FC964FF&session=0123456789abcdef0011223344556677",
        // 会话编号写法不对。
        "runode://session?machine=MACHINE&session=xyz",
        // 别的链接。
        "runode://somewhere?machine=MACHINE",
        "https://runode.dev/session",
    ])
    func otherLinksAreIgnored(_ text: String) async throws {
        let app = await app()
        app.open(url: try #require(URL(string: text.replacingOccurrences(of: "MACHINE", with: machine.id.uuidString))))
        #expect(app.path.isEmpty)
        #expect(app.pairing == nil)
    }

    /// 配对链接只填进配对页，不自动配：任何网页、短信都能发这种链接。
    @Test func pairingLinksOpenPairingWithoutPairing() async throws {
        let app = await app()
        let fp = Base64URL.encode(Data(repeating: 1, count: 32))
        let secret = Base64URL.encode(Data(repeating: 2, count: 32))
        let text = "runode://pair?v=1&name=evil&fp=\(fp)&secret=\(secret)&port=7866&addr=203.0.113.9&exp=4000000000"
        app.open(url: try #require(URL(string: text)))
        let pairing = try #require(app.pairing)
        #expect(pairing.linkText == text)
        #expect(pairing.invitation?.hostName == "evil")
        try await Task.sleep(for: .milliseconds(20))
        #expect(pairing.phase == .idle)
    }
}
