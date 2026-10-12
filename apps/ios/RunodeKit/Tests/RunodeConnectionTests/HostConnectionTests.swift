import CryptoKit
import Foundation
import RunodeProtocol
import Synchronization
import Testing

@testable import RunodeConnection

/// 连接 actor：门禁加 `Hello`，事件的先后，发的东西按先后写出，断线重连，不能重试的失败停下。
@Suite struct HostConnectionTests {
    let machine = MachineRecord.sample()
    let keys = MemoryDeviceKeyStore()
    let store = MemoryMachineStore()
    let supply = TransportSupply()

    init() throws {
        try keys.save(StoredDeviceKey(kind: .software, data: P256.Signing.PrivateKey().rawRepresentation), for: machine.id)
    }

    func connection() -> HostConnection {
        HostConnection(
            machine: machine, keyStore: keys, machines: store, discovery: NoDiscovery(),
            identity: ClientIdentity(build: "ios-test", deviceName: "测试 iPhone"),
            policy: ReconnectPolicy(base: .milliseconds(10), maximum: .milliseconds(20)), open: supply.opener)
    }

    /// 扮演宿主走完门禁和握手，返回 `Hello` 的内容。
    func admit(_ host: inout FakeHost, welcome: String = FakeHost.welcome) async throws -> [String: Any] {
        host.challenge()
        let auth = try #require(try await host.readControl())
        #expect(auth["type"] as? String == "remote_auth")
        host.send(.accepted(deviceId: machine.deviceId))
        let hello = try #require(try await host.readControl())
        host.send(json: welcome)
        return hello
    }

    /// 等到 `ready`，返回它的 generation。
    func waitReady(_ iterator: inout AsyncStream<HostEvent>.Iterator) async -> UInt64? {
        while let event = await iterator.next() {
            if case .ready(let generation) = event { return generation }
        }
        return nil
    }

    /// `keepAwake` 只发给在 `Welcome` 里说认得它的宿主：旧宿主回的 `Error` 不带编号，会被别的页面认错。
    @Test(arguments: [false, true]) func keepAwakeOnlyGoesToHostsThatKnowIt(_ knows: Bool) async throws {
        let transport = FakeTransport()
        supply.add(transport)
        let link = connection()
        var events = await link.events().makeAsyncIterator()
        await link.start()
        var host = FakeHost(transport)
        let welcome =
            knows ? FakeHost.welcome.replacingOccurrences(of: "}", with: #","keep_awake":true}"#) : FakeHost.welcome
        _ = try await admit(&host, welcome: welcome)
        _ = try #require(await waitReady(&events))
        link.send(.keepAwake)
        link.send(.listSessions)
        let first = try await host.readControl()?["type"] as? String
        #expect(first == (knows ? "keep_awake" : "list_sessions"))
        await link.stop()
    }

    @Test func connectsSaysHelloAndForwardsMessages() async throws {
        // 连接开着时用户改了名字：记地址不能把名字改回去。
        var renamed = machine
        renamed.name = "改过名"
        await store.upsert(renamed)
        let transport = FakeTransport()
        supply.add(transport)
        let link = connection()
        var events = await link.events().makeAsyncIterator()
        await link.start()
        var host = FakeHost(transport)
        let hello = try await admit(&host)
        #expect(hello["type"] as? String == "hello")
        #expect(hello["client"] as? String == "mobile")
        #expect(hello["device"] as? String == "测试 iPhone")
        #expect((hello["caps"] as? [String: Bool]) == ["snapshot": false, "vt_replay": true])
        let generation = try #require(await waitReady(&events))

        link.send(.listSessions)
        #expect(try await host.readControl()?["type"] as? String == "list_sessions")
        // 旧连接的输入丢掉，当前的照常写出。
        link.sendInput(Data("stale".utf8), channel: 1, generation: generation + 1)
        link.sendInput(Data("ls\r".utf8), channel: 1, generation: generation)
        let input = try #require(try await host.readFrame())
        #expect(input.kind == .input)
        #expect(input.payload == Data("ls\r".utf8))

        host.send(json: #"{"type":"bell","id":"0123456789abcdef0011223344556677"}"#)
        host.send(frame: Frame(kind: .output, channel: 3, payload: Data("hi".utf8)))
        var sawBell = false
        while let event = await events.next() {
            if case .message(.bell) = event { sawBell = true }
            if case .frame(let frame, let frameGeneration) = event {
                #expect(frame.payload == Data("hi".utf8))
                #expect(frameGeneration == generation)
                break
            }
        }
        #expect(sawBell)
        // 记下实际连上的地址。
        #expect(await store.all().first?.lastAddress == "192.168.1.20")
        #expect(await store.all().first?.name == "改过名")
        await link.stop()
    }

    @Test func helloCarriesTheRenamedDevice() async throws {
        let transport = FakeTransport()
        supply.add(transport)
        let link = connection()
        await link.setDeviceName("书房的 iPhone")
        await link.start()
        var host = FakeHost(transport)
        let hello = try await admit(&host)
        #expect(hello["device"] as? String == "书房的 iPhone")
        await link.stop()
    }

    @Test func reconnectsAfterTheHostGoesAway() async throws {
        let first = FakeTransport()
        let second = FakeTransport()
        supply.add(first)
        supply.add(second)
        let link = connection()
        var events = await link.events().makeAsyncIterator()
        await link.start()
        var host = FakeHost(first)
        _ = try await admit(&host)
        let generation = try #require(await waitReady(&events))
        first.close()
        var host2 = FakeHost(second)
        _ = try await admit(&host2)
        let next = try #require(await waitReady(&events))
        #expect(next == generation + 1)
        await link.stop()
    }

    /// `unknown_device`（电脑上撤销了这台设备）不自动重连：不换下一个地址、不按退避重试，App 回到前台
    /// 再 `start` 也不连；只有用户点重试（`reconnectNow`）才再试一次。电脑把这类失败计入限速。
    @Test func aRevokedDeviceStopsRetrying() async throws {
        let transport = FakeTransport()
        supply.add(transport)
        let link = connection()
        var events = await link.events().makeAsyncIterator()
        await link.start()
        var host = FakeHost(transport)
        host.challenge()
        _ = try await host.readControl()
        host.send(.rejected(.unknownDevice))
        var failure: LinkFailure?
        while let event = await events.next() {
            if case .state(.failed(let reason)) = event {
                failure = reason
                break
            }
        }
        #expect(failure == .rejected(.unknownDevice))
        // 退避只有 10 毫秒，等它十几倍的时间也没有再连。
        try await Task.sleep(for: .milliseconds(150))
        #expect(supply.requestedTargets == ["192.168.1.20"])
        // App 回到前台时会再 `start`：照样不连。
        await link.start()
        try await Task.sleep(for: .milliseconds(150))
        #expect(supply.requestedTargets == ["192.168.1.20"])
        // 用户点了重试才再连。
        let retry = FakeTransport()
        supply.add(retry)
        await link.reconnectNow()
        var retryHost = FakeHost(retry)
        _ = try await admit(&retryHost)
        #expect(await waitReady(&events) != nil)
        #expect(supply.requestedTargets == ["192.168.1.20", "192.168.1.20"])
        await link.stop()
    }

    /// 握手中途 `stop`（App 退到后台）：卡在门禁上读的那一步也要停下，回到 `idle`，不一直停在「连接中」。
    @Test func stoppingDuringTheHandshakeGivesUp() async throws {
        let transport = FakeTransport()
        supply.add(transport)
        let link = connection()
        let events = await link.events()
        let states = Mutex<[LinkState]>([])
        let watcher = Task {
            for await event in events {
                if case .state(let state) = event { states.withLock { $0.append(state) } }
            }
        }
        defer { watcher.cancel() }
        await link.start()
        // 宿主一直不发 challenge：连接开好以后停。
        while supply.requestedTargets.isEmpty {
            try await Task.sleep(for: .milliseconds(5))
        }
        await link.stop()
        // 门禁的超时是 10 秒，两秒内回到 `idle` 说明是取消让它停下的。
        func stopped() -> Bool { states.withLock { $0.drop { $0 != .connecting }.contains(.idle) } }
        for _ in 0..<200 where !stopped() {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(stopped())
    }

    @Test func onlyRateLimitsAreRetried() {
        for reason in [GateRejection.unknownDevice, .badSignature, .pairingInvalid, .disabled] {
            #expect(LinkFailure.rejected(reason).isFatal, "\(reason)")
        }
        #expect(!LinkFailure.rejected(.rateLimited).isFatal)
        #expect(!LinkFailure.rejected(.unknown("future")).isFatal)
        #expect(!LinkFailure.timeout.isFatal)
    }

    @Test func triesTheNextAddressWhenOneFails() async throws {
        let transport = FakeTransport()
        // 第一个地址打不开（没有准备连接），第二个连上。
        let opener = supply.opener
        let flaky = HostConnection(
            machine: machine, keyStore: keys, machines: store, discovery: NoDiscovery(),
            identity: ClientIdentity(build: "ios-test", deviceName: "测试 iPhone"),
            policy: ReconnectPolicy(base: .milliseconds(10), maximum: .milliseconds(20)),
            open: { target, fingerprint in
                if target.label == "192.168.1.20" { throw LinkFailure.connectionFailed("refused") }
                return try await opener(target, fingerprint)
            })
        supply.add(transport)
        var events = await flaky.events().makeAsyncIterator()
        await flaky.start()
        var host = FakeHost(transport)
        _ = try await admit(&host)
        #expect(await waitReady(&events) != nil)
        #expect(supply.requestedTargets == ["fd7a::1"])
        await flaky.stop()
    }

    /// 组播不通、Bonjour 一直找不到时，记着的地址照常马上连，不等 Bonjour 的超时。
    @Test func storedAddressesDoNotWaitForBonjour() async throws {
        struct SlowDiscovery: HostDiscovery {
            func locate(_ fingerprint: CertificateFingerprint, timeout: Duration) async -> TransportTarget? {
                try? await Task.sleep(for: .seconds(30))
                return nil
            }
        }
        let link = HostConnection(
            machine: machine, keyStore: keys, machines: store, discovery: SlowDiscovery(),
            identity: ClientIdentity(build: "ios-test", deviceName: "测试 iPhone"),
            policy: ReconnectPolicy(base: .milliseconds(10), maximum: .milliseconds(20)), open: supply.opener)
        let transport = FakeTransport()
        supply.add(transport)
        var events = await link.events().makeAsyncIterator()
        let started = ContinuousClock.now
        await link.start()
        var host = FakeHost(transport)
        _ = try await admit(&host)
        #expect(await waitReady(&events) != nil)
        #expect(ContinuousClock.now - started < .seconds(5))
        await link.stop()
    }

    /// 一次读到的帧里，同一通道上连着的输出拼成一帧交出去；控制消息和别的通道把它隔开，先后不变。
    @Test func mergesConsecutiveOutputFrames() async throws {
        let link = connection()
        let transport = FakeTransport()
        supply.add(transport)
        var events = await link.events().makeAsyncIterator()
        await link.start()
        var host = FakeHost(transport)
        _ = try await admit(&host)
        #expect(await waitReady(&events) != nil)
        func output(_ channel: UInt32, _ text: String) throws -> Data {
            try Frame(kind: .output, channel: channel, payload: Data(text.utf8)).encoded()
        }
        let bell = try Frame(
            kind: .control, channel: 0, payload: Data(#"{"type":"bell","id":"0123456789abcdef0011223344556677"}"#.utf8)
        ).encoded()
        transport.inbound.push(try output(1, "ab") + output(1, "cd") + bell + output(1, "ef") + output(2, "gh"))
        var seen: [String] = []
        while seen.count < 4, let event = await events.next() {
            switch event {
            case .frame(let frame, _): seen.append("\(frame.channel):\(String(decoding: frame.payload, as: UTF8.self))")
            case .message(.bell): seen.append("bell")
            default: break
            }
        }
        #expect(seen == ["1:abcd", "bell", "1:ef", "2:gh"])
        await link.stop()
    }

    /// 订阅者不取输出、积压过了上限时不再往下读，积压留在网络和宿主那边；取走以后接着读，一帧不丢。
    @Test func stopsReadingWhileASubscriberFallsBehind() async throws {
        let link = connection()
        let transport = FakeTransport()
        supply.add(transport)
        var events = await link.events().makeAsyncIterator()
        await link.start()
        var host = FakeHost(transport)
        _ = try await admit(&host)
        #expect(await waitReady(&events) != nil)
        let chunk = try Frame(kind: .output, channel: 1, payload: Data(count: 64 << 10)).encoded()
        for _ in 0..<16 { transport.inbound.push(chunk) }
        try await Task.sleep(for: .milliseconds(200))
        #expect(transport.inbound.pending > 8)
        var frames = 0
        while frames < 16, let event = await events.next() {
            if case .frame = event { frames += 1 }
        }
        #expect(frames == 16)
        #expect(transport.inbound.pending == 0)
        await link.stop()
    }

    @Test func missingKeyIsFatal() async throws {
        try keys.deleteKey(for: machine.id)
        let link = connection()
        var events = await link.events().makeAsyncIterator()
        await link.start()
        while let event = await events.next() {
            if case .state(.failed(let reason)) = event {
                #expect(reason == .missingKey)
                break
            }
        }
    }

    @Test func backoffGrowsAndCaps() {
        let policy = ReconnectPolicy(base: .seconds(1), maximum: .seconds(15))
        #expect(policy.delay(afterFailures: 0) == .zero)
        #expect(policy.delay(afterFailures: 1) == .seconds(1))
        #expect(policy.delay(afterFailures: 3) == .seconds(4))
        #expect(policy.delay(afterFailures: 10) == .seconds(15))
        #expect(policy.delay(afterFailures: 3, focused: false) == .seconds(4))
        #expect(policy.delay(afterFailures: 6, focused: false) == .seconds(32))
        #expect(policy.delay(afterFailures: 10, focused: false) == .seconds(60))
    }

    /// 不在看的电脑连不上时退避到 `idleMaximum`；变回在看时不等这么久，马上重试。
    @Test func focusingAnIdleMachineRetriesAtOnce() async throws {
        let link = HostConnection(
            machine: machine, keyStore: keys, machines: store, discovery: NoDiscovery(),
            identity: ClientIdentity(build: "ios-test", deviceName: "测试 iPhone"),
            // 第一次失败就等 5 秒，比 `maximum` 长得多，测试不用等它一档档涨上去。
            policy: ReconnectPolicy(base: .seconds(5), maximum: .milliseconds(20), idleMaximum: .seconds(30)),
            open: supply.opener)
        link.setFocused(false)
        var events = await link.events().makeAsyncIterator()
        await link.start()
        // 没有准备连接，连不上。
        while let event = await events.next() {
            if case .state(.waiting(_, let retryAt)) = event, retryAt.timeIntervalSinceNow > 2 { break }
        }
        let transport = FakeTransport()
        supply.add(transport)
        let started = ContinuousClock.now
        link.setFocused(true)
        var host = FakeHost(transport)
        _ = try await admit(&host)
        #expect(await waitReady(&events) != nil)
        #expect(ContinuousClock.now - started < .seconds(2))
        await link.stop()
    }
}

@Suite struct MachineTests {
    @Test func fallbackTargetsPutTheLastAddressFirst() {
        var machine = MachineRecord.sample()
        machine.lastAddress = "fd7a::1"
        #expect(machine.fallbackTargets.map(\.label) == ["fd7a::1", "192.168.1.20"])
    }

    @Test func fileStoreRoundTrips() async throws {
        let url = FileManager.default.temporaryDirectory.appending(path: "runode-test-\(UUID().uuidString)/machines.json")
        let store = FileMachineStore(url: url)
        var machine = MachineRecord.sample()
        try await store.upsert(machine)
        machine.name = "改过名"
        try await store.upsert(machine)
        let reloaded = await FileMachineStore(url: url).all()
        #expect(reloaded == [machine])
        try await store.remove(id: machine.id)
        #expect(await FileMachineStore(url: url).all().isEmpty)
        try? FileManager.default.removeItem(at: url.deletingLastPathComponent())
    }

    @Test func updatingTheAddressKeepsTheNewName() async throws {
        let url = FileManager.default.temporaryDirectory.appending(path: "runode-test-\(UUID().uuidString)/machines.json")
        defer { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
        let store = FileMachineStore(url: url)
        var machine = MachineRecord.sample()
        try await store.upsert(machine)
        machine.name = "改过名"
        try await store.upsert(machine)
        try await store.updateLastAddress(id: machine.id, "fd7a::1")
        machine.lastAddress = "fd7a::1"
        #expect(await FileMachineStore(url: url).all() == [machine])
    }

    @Test func updatingTheAddressOfADeletedMachineWritesNothing() async throws {
        let url = FileManager.default.temporaryDirectory.appending(path: "runode-test-\(UUID().uuidString)/machines.json")
        defer { try? FileManager.default.removeItem(at: url.deletingLastPathComponent()) }
        let store = FileMachineStore(url: url)
        let machine = MachineRecord.sample()
        try await store.upsert(machine)
        try await store.remove(id: machine.id)
        try await store.updateLastAddress(id: machine.id, "fd7a::1")
        #expect(await FileMachineStore(url: url).all().isEmpty)

        let memory = MemoryMachineStore()
        await memory.updateLastAddress(id: machine.id, "fd7a::1")
        #expect(await memory.all().isEmpty)
    }
}
