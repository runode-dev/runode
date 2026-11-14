package dev.runode.mobile.features.model

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.runode.mobile.connection.DeviceKeyStore
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.connection.MachineStore
import dev.runode.mobile.features.R
import java.time.Duration
import java.time.Instant
import java.util.UUID
import kotlinx.coroutines.CancellationException

/** 配对过的电脑：列出、改名、删除（连同 Keystore 里的设备私钥）。 */
class MachineListModel(
    private val store: MachineStore,
    private val keyStore: DeviceKeyStore,
    private val now: () -> Instant = { Instant.now() },
) {
    var machines by mutableStateOf<List<MachineRecord>>(emptyList())
        private set

    /** 删掉了、等连上注销推送的电脑（`MachineRecord.deletedAt`），不在 [machines] 里。 */
    internal var leaving by mutableStateOf<List<MachineRecord>>(emptyList())
        private set

    var loaded by mutableStateOf(false)
        private set

    var errorMessage by mutableStateOf<UiText?>(null)

    /** 正在改名的那台；输入框里的名字是 [renameText]。 */
    var renameTarget by mutableStateOf<UUID?>(null)
        private set
    var renameText by mutableStateOf("")

    /** 等用户确认删除的那台。 */
    var deleteTarget by mutableStateOf<UUID?>(null)

    /**
     * 删掉一台电脑之前先请电脑忘掉这部手机（解除配对，电脑不认得时只注销推送）、断开它的连接、退出它的页面；电脑那边
     * 办好了（或者本来就没登记推送）时为真。
     */
    var willDelete: suspend (UUID) -> Boolean = { true }

    /** 每次重新读完列表（配对、改名、删除之后都会读）以后调，[AppModel] 据此给每台电脑建好连接。 */
    var didLoad: () -> Unit = {}

    suspend fun load() {
        val all = store.all().sortedBy { it.pairedAt }.toMutableList()
        for (machine in all.toList()) {
            val deletedAt = machine.deletedAt ?: continue
            if (Duration.between(deletedAt, now()) <= leavingLimit) continue
            if (runCatchingNonCancel { remove(machine.id) }) all.removeAll { it.id == machine.id }
        }
        machines = all.filter { it.deletedAt == null }
        leaving = all.filter { it.deletedAt != null }
        loaded = true
        didLoad()
    }

    fun machine(id: UUID): MachineRecord? = machines.firstOrNull { it.id == id }

    /**
     * 配对好的一台存下来；同一个证书指纹的旧记录（重新配对的）连同私钥一起换掉。新的存好了才动旧的：存不下时抛出去、
     * 丢掉配对时存的新私钥，旧的配对照样能用；旧的删不掉时两条都留着。
     */
    suspend fun add(machine: MachineRecord) {
        try {
            store.upsert(machine)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            runCatching { keyStore.deleteKey(machine.id) }
            load()
            throw e
        }
        for (old in machines) {
            if (old.fingerprint == machine.fingerprint && old.id != machine.id) retire(old)
        }
        load()
    }

    /** 改名的输入框开着；设成假时放弃。 */
    var isRenaming: Boolean
        get() = renameTarget != null
        set(value) {
            if (!value) renameTarget = null
        }

    fun beginRename(id: UUID) {
        renameText = machine(id)?.name ?: ""
        renameTarget = id
    }

    suspend fun rename(id: UUID, name: String) {
        val trimmed = name.trim()
        if (machine(id) == null || trimmed.isEmpty()) return
        try {
            store.updateName(id, trimmed)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            errorMessage = UiText.res(R.string.machine_rename_failed, e.message ?: e.toString())
        }
        load()
    }

    suspend fun delete(id: UUID) {
        machine(id)?.let { retire(it) }
        load()
    }

    /** 删掉时没注销成推送的那台（[leaving]）在电脑那边注销好了：这才删记录和私钥。删不掉就下次连上再来。 */
    internal suspend fun purge(id: UUID) {
        runCatchingNonCancel { remove(id) }
        load()
    }

    /**
     * 删掉一台。电脑没注销好推送（没连着、没回话）时只记下删掉的时刻、留着私钥：真删了手机就再也连不上它去注销，它会
     * 一直往这部手机推它的提醒。
     */
    private suspend fun retire(machine: MachineRecord) {
        val unregistered = willDelete(machine.id)
        try {
            if (unregistered) remove(machine.id) else store.upsert(machine.copy(deletedAt = now()))
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            errorMessage = UiText.res(R.string.machine_delete_failed, e.message ?: e.toString())
        }
    }

    /** 记录删掉了才删私钥：删记录失败时电脑照样能用；删私钥失败只留下一把用不上的钥匙。 */
    private suspend fun remove(id: UUID) {
        store.remove(id)
        runCatching { keyStore.deleteKey(id) }
    }

    /** 跑 [block]，成了返回真；出错返回假，协程撤销照样抛出去。 */
    private suspend fun runCatchingNonCancel(block: suspend () -> Unit): Boolean = try {
        block()
        true
    } catch (e: CancellationException) {
        throw e
    } catch (e: Exception) {
        false
    }

    internal companion object {
        /** 删掉这么久还没连上注销推送的电脑（卖了、重装了）不再等，真删掉：留着私钥只会一直拿它去连。 */
        val leavingLimit: Duration = Duration.ofDays(30)
    }
}
