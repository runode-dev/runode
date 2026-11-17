package dev.runode.mobile.connection

/**
 * [IrohDialer] 按私钥记着的端点。每个端点记着借着它的连接数（正在拨号的和拨通了还开着的都算）；超过 [limit] 个时只收
 * 没人借着的、最久没用的那些，都有人借着时暂时超出，等有人还了再收。收下来的交给调用方关。不是线程安全的，由调用方加锁。
 */
internal class TunnelEndpointPool<K : Any, E>(private val limit: Int) {
    /** 借一次端点的凭据；端点换掉或者忘掉以后，旧的凭据还回来不算数。 */
    data class Lease<K>(val key: K, val generation: Long)

    private class Entry<E>(val endpoint: E, val generation: Long, var borrowers: Int)

    /** 按最近用过的先后排，最久没用的在前。 */
    private val entries = LinkedHashMap<K, Entry<E>>()
    private var nextGeneration = 0L

    val count: Int get() = entries.size

    /** 借 [key] 记着的端点，记成刚用过；没有时为空。 */
    fun borrow(key: K): Pair<E, Lease<K>>? {
        val entry = entries[key] ?: return null
        entry.borrowers += 1
        touch(key, entry)
        return entry.endpoint to Lease(key, entry.generation)
    }

    /** 记下 [key] 的新端点并借走它。返回凭据和要关掉的端点：同一个键原来记着的，和超出上限收下来的。 */
    fun insert(endpoint: E, key: K): Pair<Lease<K>, List<E>> {
        val evicted = mutableListOf<E>()
        entries.remove(key)?.let { evicted += it.endpoint }
        nextGeneration += 1
        entries[key] = Entry(endpoint, nextGeneration, borrowers = 1)
        evicted += evict()
        return Lease(key, nextGeneration) to evicted
    }

    /** 还回 [lease]，记成刚用过。返回超出上限收下来的端点。 */
    fun giveBack(lease: Lease<K>): List<E> {
        val entry = entries[lease.key]?.takeIf { it.generation == lease.generation } ?: return emptyList()
        entry.borrowers = maxOf(0, entry.borrowers - 1)
        touch(lease.key, entry)
        return evict()
    }

    /** 忘掉 [lease] 借的那个端点（建失败了）；已经换成别的时不动。 */
    fun remove(lease: Lease<K>): E? {
        if (entries[lease.key]?.generation != lease.generation) return null
        return remove(lease.key)
    }

    /** 忘掉 [key] 记着的端点，不管有没有人借着。 */
    fun remove(key: K): E? = entries.remove(key)?.endpoint

    private fun touch(key: K, entry: Entry<E>) {
        entries.remove(key)
        entries[key] = entry
    }

    private fun evict(): List<E> {
        val evicted = mutableListOf<E>()
        while (entries.size > limit) {
            val idle = entries.entries.firstOrNull { it.value.borrowers == 0 } ?: break
            entries.remove(idle.key)
            evicted += idle.value.endpoint
        }
        return evicted
    }
}
