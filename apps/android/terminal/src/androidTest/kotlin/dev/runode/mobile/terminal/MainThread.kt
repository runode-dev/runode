package dev.runode.mobile.terminal

import androidx.test.platform.app.InstrumentationRegistry

/** 在主线程上跑 [block]（[VTerminal] 只能在主线程上用），把结果或抛出的异常带回测试线程。 */
fun <T> valueOnMain(block: () -> T): T {
    var result: Result<T>? = null
    InstrumentationRegistry.getInstrumentation().runOnMainSync { result = runCatching(block) }
    return result!!.getOrThrow()
}

/** 不要结果的 [valueOnMain]，测试方法写成 `= onMain { … }` 时返回的是 Unit。 */
fun onMain(block: () -> Unit) = valueOnMain(block)
