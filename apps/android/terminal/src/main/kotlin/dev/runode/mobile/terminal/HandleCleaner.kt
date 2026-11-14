package dev.runode.mobile.terminal

import java.lang.ref.PhantomReference
import java.lang.ref.ReferenceQueue
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.concurrent.thread

/**
 * 对象被回收后跑一段清理，兜住忘了 `close` 的 native 句柄。清理在一条后台守护线程上跑，清理动作不能引用要看着的对象，
 * 否则它永远回收不了。
 *
 * shortcut: 自己用 PhantomReference 做的 [java.lang.ref.Cleaner]，minSdk 升到 33 后换成它。
 */
internal object HandleCleaner {
    private val queue = ReferenceQueue<Any>()

    /** 还没清理的，留着引用免得 PhantomReference 自己先被回收。 */
    private val pending: MutableSet<Cleanable> = ConcurrentHashMap.newKeySet()

    class Cleanable internal constructor(owner: Any, private val action: Runnable) :
        PhantomReference<Any>(owner, queue) {
        private val done = AtomicBoolean(false)

        /** 跑清理动作，只跑一次；主动调了以后对象回收时不再跑。 */
        fun clean() {
            if (!done.compareAndSet(false, true)) return
            pending.remove(this)
            clear()
            action.run()
        }
    }

    init {
        thread(isDaemon = true, name = "runode-handle-cleaner") {
            while (true) {
                try {
                    (queue.remove() as Cleanable).clean()
                } catch (_: InterruptedException) {
                    // 守护线程不会被别人打断；真打断了接着等。
                }
            }
        }
    }

    /** [owner] 被回收后跑 [action]。 */
    fun register(owner: Any, action: Runnable): Cleanable = Cleanable(owner, action).also { pending += it }
}
