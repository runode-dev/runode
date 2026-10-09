import Foundation
import RunodeActivity
import RunodeConnection
import RunodeProtocol
import Testing

@testable import RunodeFeatures

/// 假的 Live Activity 系统接口：token 和系统开关由测试推，记下收起了哪些。
@MainActor
final class FakeLiveActivities: LiveActivities {
    var areEnabled = true
    var shown: [AgentActivityAttributes] = []
    var ended: [AgentActivityAttributes] = []
    private let tokens: AsyncStream<Data>
    private let tokenSink: AsyncStream<Data>.Continuation
    private let enablement: AsyncStream<Bool>
    private let enablementSink: AsyncStream<Bool>.Continuation

    init() {
        (tokens, tokenSink) = AsyncStream.makeStream(of: Data.self)
        (enablement, enablementSink) = AsyncStream.makeStream(of: Bool.self)
    }

    func pushToStartTokens() -> AsyncStream<Data> { tokens }
    func enablementUpdates() -> AsyncStream<Bool> { enablement }

    func give(token: Data) { tokenSink.yield(token) }
    func allow(_ allowed: Bool) { enablementSink.yield(allowed) }

    func end(machine: String, session: String) async {
        ended += shown.filter { $0.machine == machine && $0.session == session }
        shown.removeAll { $0.machine == machine && $0.session == session }
    }
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

let pushIdentity = PushIdentity(environment: .development, bundle: "dev.runode.mobile")
private let firstToken = Data([0x80, 0xf0, 0x0a])
private let secondToken = Data([0x01, 0x02])

@MainActor
@Suite struct PushRegistrationTests {
    let system = FakeLiveActivities()
    let machine = machineRecord(name: "书房的 Mac")
    let link = FakeLink()

    func registration(
        enabled: Bool = true, identity: PushIdentity? = pushIdentity, timeout: Duration = .seconds(5)
    ) -> PushRegistration {
        PushRegistration(system: system, identity: identity, enabled: enabled, replyTimeout: timeout)
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

    /// 老版本的 runode 回不带编号的「unknown message」或者干脆不回：等到时候就当它不支持。
    @Test func noReplyMeansUnsupported() async {
        let push = registration(timeout: .milliseconds(50))
        system.give(token: firstToken)
        #expect(await eventually { push.token != nil })
        push.connected(machine, link: link)
        #expect(await eventually { link.registrations.count == 1 })
        push.handle(.error(req: nil, id: nil, message: HostMsg.unknownMessage), from: machine.id)
        #expect(push.statuses[machine.id] == nil)
        #expect(await eventually { push.statuses[machine.id] == .unsupported })
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

    @Test func withoutAnIdentityNothingIsSent() async {
        let push = registration(identity: nil)
        system.give(token: firstToken)
        #expect(await eventually { push.token != nil })
        push.connected(machine, link: link)
        try? await Task.sleep(for: .milliseconds(20))
        #expect(link.registrations.isEmpty)
    }

    @Test func followsTheSystemSwitch() async {
        system.areEnabled = false
        let push = registration()
        #expect(!push.systemAllowed)
        system.allow(true)
        #expect(await eventually { push.systemAllowed })
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

/// `AppModel` 把推送登记、本地收起和深链接接到各台电脑的连接上。
@MainActor
@Suite struct AppPushTests {
    let machine = machineRecord()
    let link = FakeLink()
    let system = FakeLiveActivities()
    let preferences = DefaultsStore<AppPreferences>("preferences", defaults: .temporary())

    func app(load: Bool = true) async -> AppModel {
        let store = MemoryMachineStore()
        await store.upsert(machine)
        let link = self.link
        var dependencies = AppDependencies(
            store: store, keyStore: MemoryDeviceKeyStore(), pairing: FakePairing { _ in machineRecord() },
            makeLink: { _ in link }, deviceName: "测试 iPhone", preferences: preferences)
        dependencies.liveActivities = system
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

    func activity(_ session: SessionId, machine: UUID? = nil) -> AgentActivityAttributes {
        AgentActivityAttributes(
            machine: (machine ?? self.machine.id).uuidString, machineName: "MacBook", session: session.rawValue)
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

    /// 没连着的电脑删掉时不发注销，也不等。
    @Test func deletingADisconnectedMachineDoesNotWait() async throws {
        let app = await app()
        system.give(token: Data([0xab]))
        #expect(await eventually { app.push.token == "ab" })
        await app.machineList.delete(machine.id)
        #expect(app.machineList.machines.isEmpty)
        #expect(link.registrations.isEmpty)
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

    @Test func answeredActivitiesAreEndedInTheForeground() async throws {
        let app = await app()
        let elsewhere = UUID()
        system.shown = [
            activity(sessionA), activity(sessionB), activity(sessionC), activity(sessionA, machine: elsewhere),
        ]
        let list = try #require(app.sessionList(for: machine.id))
        connect(list)
        // A 已经不等了，B 还在等，C 没了；别的电脑上的不动。
        list.handle(.message(.sessionList([info(sessionA, .working), info(sessionB, .blocked)])))
        #expect(await eventually { system.ended.count == 2 })
        #expect(Set(system.ended) == [activity(sessionA), activity(sessionC)])
        #expect(system.shown == [activity(sessionB), activity(sessionA, machine: elsewhere)])
        // 答完了也收起。
        list.handle(.message(.meta(id: sessionB, meta: info(sessionB, .working).meta)))
        #expect(await eventually { system.ended.count == 3 })
    }

    /// 重连上还没收到新列表时手上是断开前的列表，不能拿它判断；在后台时也不动。
    @Test func staleListsAndTheBackgroundLeaveActivitiesAlone() async throws {
        let app = await app()
        let list = try #require(app.sessionList(for: machine.id))
        connect(list)
        list.handle(.message(.sessionList([info(sessionA, .working)])))
        list.handle(.state(.waiting(reason: "断了", retryAt: .now)))
        system.shown = [activity(sessionA)]
        connect(list)
        list.handle(.message(.meta(id: sessionA, meta: info(sessionA, .working).meta)))
        try await Task.sleep(for: .milliseconds(20))
        #expect(system.ended.isEmpty)
        app.setActive(false)
        list.handle(.message(.sessionList([info(sessionA, .working)])))
        try await Task.sleep(for: .milliseconds(20))
        #expect(system.ended.isEmpty)
        app.setActive(true)
        list.handle(.message(.sessionList([info(sessionA, .working)])))
        #expect(await eventually { system.ended == [activity(sessionA)] })
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
