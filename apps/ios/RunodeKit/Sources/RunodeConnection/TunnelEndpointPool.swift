import Foundation

/// `IrohDialer` 按私钥记着的端点。每个端点记着借着它的连接数（正在拨号的和拨通了还开着的都算）；超过
/// `limit` 个时只收没人借着的、最久没用的那些，都有人借着时暂时超出，等有人还了再收。收下来的交给调用方关。
struct TunnelEndpointPool<Endpoint> {
    /// 借一次端点的凭据；端点换掉或者忘掉以后，旧的凭据还回来不算数。
    struct Lease: Hashable, Sendable {
        let secret: Data
        let generation: UInt64
    }

    private struct Entry {
        var endpoint: Endpoint
        var generation: UInt64
        var borrowers: Int
    }

    let limit: Int
    private var entries: [Data: Entry] = [:]
    /// `entries` 的私钥，最久没用的在前。
    private var recent: [Data] = []
    private var nextGeneration: UInt64 = 0

    init(limit: Int) {
        self.limit = limit
    }

    var count: Int { entries.count }

    /// 借 `secret` 记着的端点，记成刚用过；没有时为空。
    mutating func borrow(_ secret: Data) -> (endpoint: Endpoint, lease: Lease)? {
        guard var entry = entries[secret] else { return nil }
        entry.borrowers += 1
        entries[secret] = entry
        touch(secret)
        return (entry.endpoint, Lease(secret: secret, generation: entry.generation))
    }

    /// 记下 `secret` 的新端点并借走它。返回凭据和要关掉的端点：同一把私钥原来记着的，和超出上限收下来的。
    mutating func insert(_ endpoint: Endpoint, for secret: Data) -> (lease: Lease, evicted: [Endpoint]) {
        var evicted: [Endpoint] = []
        if let old = entries[secret] { evicted.append(old.endpoint) }
        nextGeneration += 1
        entries[secret] = Entry(endpoint: endpoint, generation: nextGeneration, borrowers: 1)
        touch(secret)
        evicted += evict()
        return (Lease(secret: secret, generation: nextGeneration), evicted)
    }

    /// 还回 `lease`，记成刚用过。返回超出上限收下来的端点。
    mutating func giveBack(_ lease: Lease) -> [Endpoint] {
        guard var entry = entries[lease.secret], entry.generation == lease.generation else { return [] }
        entry.borrowers = max(0, entry.borrowers - 1)
        entries[lease.secret] = entry
        touch(lease.secret)
        return evict()
    }

    /// 忘掉 `lease` 借的那个端点（建失败了或者已经关了）；已经换成别的时不动。
    mutating func remove(_ lease: Lease) -> Endpoint? {
        guard entries[lease.secret]?.generation == lease.generation else { return nil }
        return remove(lease.secret)
    }

    /// 忘掉 `secret` 记着的端点，不管有没有人借着。
    mutating func remove(_ secret: Data) -> Endpoint? {
        recent.removeAll { $0 == secret }
        return entries.removeValue(forKey: secret)?.endpoint
    }

    private mutating func touch(_ secret: Data) {
        recent.removeAll { $0 == secret }
        recent.append(secret)
    }

    private mutating func evict() -> [Endpoint] {
        var evicted: [Endpoint] = []
        while entries.count > limit, let idle = recent.first(where: { entries[$0]?.borrowers == 0 }) {
            if let endpoint = remove(idle) { evicted.append(endpoint) }
        }
        return evicted
    }
}
