import Foundation
import Testing

@testable import RunodeConnection

/// iroh 端点的去留：超出上限时只收没人借着的、最久没用的；都有人借着时等还回来再收。
@Suite struct TunnelEndpointPoolTests {
    func secret(_ byte: UInt8) -> Data { Data(repeating: byte, count: 32) }

    @Test func evictsTheLeastRecentlyUsedIdleEndpoint() {
        var pool = TunnelEndpointPool<String>(limit: 2)
        let a = pool.insert("a", for: secret(1))
        #expect(pool.giveBack(a.lease).isEmpty)
        let b = pool.insert("b", for: secret(2))
        #expect(pool.giveBack(b.lease).isEmpty)
        // 再借一次 a，最久没用的就成了 b。
        let again = pool.borrow(secret(1))
        #expect(again?.endpoint == "a")
        #expect(pool.giveBack(again!.lease).isEmpty)
        let c = pool.insert("c", for: secret(3))
        #expect(c.evicted == ["b"])
        #expect(pool.borrow(secret(2)) == nil)
    }

    @Test func endpointsInUseAreNotEvicted() {
        var pool = TunnelEndpointPool<String>(limit: 2)
        let a = pool.insert("a", for: secret(1))
        let b = pool.insert("b", for: secret(2))
        #expect(pool.giveBack(b.lease).isEmpty)
        // a 最久没用，但还有连接借着：收的是 b。
        #expect(pool.insert("c", for: secret(3)).evicted == ["b"])
        #expect(pool.borrow(secret(1))?.endpoint == "a")
        _ = a
    }

    @Test func goesOverTheLimitUntilAConnectionEnds() {
        var pool = TunnelEndpointPool<String>(limit: 1)
        let a = pool.insert("a", for: secret(1))
        let b = pool.insert("b", for: secret(2))
        #expect(b.evicted.isEmpty)
        #expect(pool.count == 2)
        #expect(pool.giveBack(a.lease) == ["a"])
        #expect(pool.count == 1)
        #expect(pool.giveBack(b.lease).isEmpty)
    }

    /// 每条连接各借一次，都还了才算没人借着。
    @Test func countsEveryBorrower() throws {
        var pool = TunnelEndpointPool<String>(limit: 1)
        let first = pool.insert("a", for: secret(1))
        let borrowed = pool.borrow(secret(1))
        let second = try #require(borrowed)
        _ = pool.insert("b", for: secret(2))
        #expect(pool.giveBack(first.lease).isEmpty)
        #expect(pool.giveBack(second.lease) == ["a"])
    }

    /// 端点换掉以后，借旧端点的凭据还回来、要去掉它，都不碰新的。
    @Test func staleLeasesDoNotTouchTheReplacement() {
        var pool = TunnelEndpointPool<String>(limit: 1)
        let old = pool.insert("old", for: secret(1))
        #expect(pool.remove(old.lease) == "old")
        let new = pool.insert("new", for: secret(1))
        #expect(pool.remove(old.lease) == nil)
        #expect(pool.giveBack(old.lease).isEmpty)
        _ = pool.insert("b", for: secret(2))
        #expect(pool.count == 2, "new is still borrowed")
        #expect(pool.giveBack(new.lease) == ["new"])
    }

    @Test func removingBySecretIgnoresBorrowers() {
        var pool = TunnelEndpointPool<String>(limit: 2)
        let a = pool.insert("a", for: secret(1))
        #expect(pool.remove(secret(1)) == "a")
        #expect(pool.giveBack(a.lease).isEmpty)
        #expect(pool.count == 0)
    }
}
