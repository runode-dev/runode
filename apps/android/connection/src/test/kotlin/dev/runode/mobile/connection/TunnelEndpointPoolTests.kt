package dev.runode.mobile.connection

import kotlin.test.assertEquals
import kotlin.test.assertNull
import org.junit.Test

/** 按私钥记着的 iroh 端点：超过上限时收没人借着的、最久没用的。 */
class TunnelEndpointPoolTests {
    private val pool = TunnelEndpointPool<String, String>(limit = 2)

    /** 放进去、还回来；返回收下来的端点。 */
    private fun use(key: String): List<String> {
        val (lease, evicted) = pool.insert("endpoint $key", key)
        return evicted + pool.giveBack(lease)
    }

    @Test
    fun evictsTheLeastRecentlyUsedIdleEndpoint() {
        assertEquals(emptyList(), use("a"))
        assertEquals(emptyList(), use("b"))
        // 借过 a，最久没用的成了 b。
        pool.giveBack(pool.borrow("a")!!.second)
        assertEquals(listOf("endpoint b"), use("c"))
        assertEquals(2, pool.count)
        assertNull(pool.borrow("b"))
    }

    /** 都有人借着时暂时超出上限，有人还了再收。 */
    @Test
    fun keepsBorrowedEndpointsUntilTheyAreGivenBack() {
        val (a, _) = pool.insert("endpoint a", "a")
        val (b, _) = pool.insert("endpoint b", "b")
        val (c, evicted) = pool.insert("endpoint c", "c")
        assertEquals(emptyList(), evicted)
        assertEquals(3, pool.count)
        assertEquals(listOf("endpoint b"), pool.giveBack(b))
        assertEquals(2, pool.count)
        assertEquals(emptyList(), pool.giveBack(a))
        assertEquals(emptyList(), pool.giveBack(c))
    }

    /** 同一个键借几次要还几次才算没人借着。 */
    @Test
    fun countsEachBorrower() {
        val (a, _) = pool.insert("endpoint a", "a")
        val second = pool.borrow("a")!!.second
        pool.giveBack(a)
        assertEquals(emptyList(), use("b"))
        assertEquals(listOf("endpoint b"), use("c"), "a is still borrowed once")
        assertEquals(emptyList(), pool.giveBack(second))
        assertEquals("endpoint a", pool.borrow("a")?.first)
    }

    /** 端点换掉以后，旧的凭据还回来不算数。 */
    @Test
    fun aStaleLeaseIsIgnored() {
        val (old, _) = pool.insert("endpoint a", "a")
        assertEquals("endpoint a", pool.remove(old))
        val (fresh, _) = pool.insert("endpoint a2", "a")
        assertEquals(emptyList(), pool.giveBack(old))
        assertNull(pool.remove(old))
        assertEquals("endpoint a2", pool.borrow("a")!!.first)
        pool.giveBack(fresh)
    }

    /** 忘掉一个键不管有没有人借着；没记过的什么也不做。 */
    @Test
    fun removingAKeyIgnoresBorrowers() {
        val (lease, _) = pool.insert("endpoint a", "a")
        assertEquals("endpoint a", pool.remove("a"))
        assertNull(pool.remove("a"))
        assertEquals(emptyList(), pool.giveBack(lease))
        assertEquals(0, pool.count)
    }
}
