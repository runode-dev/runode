import Synchronization
import Testing

@testable import RunodeConnection

/// iroh-ffi 的调用取消了也不停：拨号要在取消时马上返回，晚连上的交给 `discard` 关掉。
@Suite(.timeLimit(.minutes(1))) struct TunnelTests {
    @Test func cancellingDoesNotWaitForTheWork() async throws {
        let (started, start) = AsyncStream.makeStream(of: Void.self)
        let (released, release) = AsyncStream.makeStream(of: Void.self)
        let (finished, done) = AsyncStream.makeStream(of: Int.self)
        let dial = Task {
            try await abandoningOnCancel {
                start.yield()
                for await _ in released { break }
                return 7
            } discard: { value in
                done.yield(value)
            }
        }
        for await _ in started { break }
        dial.cancel()
        await #expect(throws: CancellationError.self) { try await dial.value }
        release.yield()
        var discarded: Int?
        for await value in finished {
            discarded = value
            break
        }
        #expect(discarded == 7)
    }

    @Test func cancelledBeforeStartingThrows() async {
        let dial = Task {
            withUnsafeCurrentTask { $0?.cancel() }
            return try await abandoningOnCancel { 1 } discard: { _ in }
        }
        await #expect(throws: CancellationError.self) { try await dial.value }
    }

    @Test func passesTheResultThrough() async throws {
        let value = try await abandoningOnCancel { 3 } discard: { _ in Issue.record("nothing to discard") }
        #expect(value == 3)
    }
}
