package dev.runode.mobile.connection

import android.content.Context
import dev.runode.mobile.protocol.CertificateFingerprint
import java.io.File
import java.io.IOException
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import java.time.Instant
import java.util.UUID
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.serialization.KSerializer
import kotlinx.serialization.Serializable
import kotlinx.serialization.builtins.ListSerializer
import kotlinx.serialization.descriptors.PrimitiveKind
import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.json.Json

/** 一台配对过的电脑。私钥不在这里，在 [DeviceKeyStore] 里按 [id] 存。 */
@Serializable
data class MachineRecord(
    @Serializable(with = UUIDSerializer::class) val id: UUID = UUID.randomUUID(),
    /** 列表里显示的名字，用户可以改；配对时取二维码里的主机名。 */
    val name: String,
    /** 门禁时电脑报的主机名。 */
    val hostName: String,
    val fingerprint: CertificateFingerprint,
    val port: UShort,
    /** 二维码里的地址。 */
    val addresses: List<String>,
    /** 上次连上时对方的地址，下次在 Bonjour 之后先试它。 */
    val lastAddress: String?,
    /** 配对时宿主发的设备编号。 */
    val deviceId: String,
    @Serializable(with = InstantSerializer::class) val pairedAt: Instant = Instant.now(),
    /** 跨网络连接的地址（iroh 的公钥），每次连上时按电脑报的更新；为空时只能在同一个网络里连。 */
    val node: String? = null,
    /**
     * 用户删掉了它、电脑那边却还没注销这部手机的推送时，记着删掉的时刻：列表里不再显示，私钥留着，下次连上注销好了
     * 再真删。为空时没删。
     */
    @Serializable(with = InstantSerializer::class) val deletedAt: Instant? = null,
) {
    /** 发现之外要挨个试的地址：上次成功的在前，再是二维码里的，去掉重复。 */
    val fallbackTargets: List<TransportTarget>
        get() = (listOfNotNull(lastAddress) + addresses).distinct().mapNotNull { TransportTarget.address(it, port) }
}

/** 配对过的电脑存在哪里。 */
interface MachineStore {
    suspend fun all(): List<MachineRecord>

    /** 加一台，或者按 `id` 换掉已有的。 */
    suspend fun upsert(machine: MachineRecord)

    /** 只改存着的那条的 `lastAddress`，别的字段（用户改过的名字）照旧；这条已经删了时什么也不做。 */
    suspend fun updateLastAddress(id: UUID, address: String)

    /** 只改存着的那条的 `node`；这条已经删了时什么也不做。 */
    suspend fun updateNode(id: UUID, node: String)

    /** 只改存着的那条的 `name`：连接在后台记下的 `node`、`lastAddress` 照旧；这条已经删了时什么也不做。 */
    suspend fun updateName(id: UUID, name: String)

    suspend fun remove(id: UUID)
}

/** 只记在内存里，测试和演示模式用。 */
class MemoryMachineStore(machines: List<MachineRecord> = emptyList()) : MachineStore {
    private val mutex = Mutex()
    private var machines = machines

    override suspend fun all(): List<MachineRecord> = mutex.withLock { machines }

    override suspend fun upsert(machine: MachineRecord) = mutex.withLock {
        machines = machines.filter { it.id != machine.id } + machine
    }

    override suspend fun updateLastAddress(id: UUID, address: String) = update(id) { it.copy(lastAddress = address) }

    override suspend fun updateNode(id: UUID, node: String) = update(id) { it.copy(node = node) }

    override suspend fun updateName(id: UUID, name: String) = update(id) { it.copy(name = name) }

    override suspend fun remove(id: UUID) = mutex.withLock { machines = machines.filter { it.id != id } }

    private suspend fun update(id: UUID, change: (MachineRecord) -> MachineRecord) = mutex.withLock {
        machines = machines.map { if (it.id == id) change(it) else it }
    }
}

/** 存在 app 私有目录下的一个 JSON 文件里；记录里没有秘密（私钥在 Keystore）。 */
class FileMachineStore(private val file: File) : MachineStore {
    private val mutex = Mutex()
    private var cache: List<MachineRecord>? = null

    override suspend fun all(): List<MachineRecord> = locked {
        // 读不出来时给界面一张空表，下次再读；改动走 `load`，读不出来就不写。
        runCatching { load() }.getOrDefault(emptyList())
    }

    override suspend fun upsert(machine: MachineRecord) = locked {
        val machines = load()
        write(if (machines.any { it.id == machine.id }) machines.map { if (it.id == machine.id) machine else it } else machines + machine)
    }

    override suspend fun updateLastAddress(id: UUID, address: String) = update(id) { it.copy(lastAddress = address) }

    override suspend fun updateNode(id: UUID, node: String) = update(id) { it.copy(node = node) }

    override suspend fun updateName(id: UUID, name: String) = update(id) { it.copy(name = name) }

    override suspend fun remove(id: UUID) = locked { write(load().filter { it.id != id }) }

    private suspend fun update(id: UUID, change: (MachineRecord) -> MachineRecord) = locked {
        val machines = load()
        if (machines.any { it.id == id }) write(machines.map { if (it.id == id) change(it) else it })
    }

    private suspend fun <T> locked(block: () -> T): T = mutex.withLock { withContext(Dispatchers.IO) { block() } }

    /**
     * 读出存着的全部记录。读不出来（出了 I/O 错）不等于文件坏了：不记缓存也不动文件，抛出去，下次再读；改动拿它垫底，
     * 读不出来就不写，不然会把别的配对冲掉。
     */
    private fun load(): List<MachineRecord> {
        cache?.let { return it }
        if (!file.exists()) return emptyList<MachineRecord>().also { cache = it }
        val text = file.readText()
        val machines = try {
            json.decodeFromString(serializer, text)
        } catch (_: RuntimeException) {
            // 文件在、读得出却解不出来（坏了、格式对不上）：挪开留底，不当成空表，不然下一次写就把所有配对冲掉。
            val backup = File(file.path + ".corrupt")
            backup.delete()
            file.renameTo(backup)
            emptyList()
        }
        cache = machines
        return machines
    }

    private fun write(machines: List<MachineRecord>) {
        val directory = file.absoluteFile.parentFile ?: throw IOException("no directory for $file")
        if (!directory.isDirectory && !directory.mkdirs()) throw IOException("cannot create $directory")
        val temporary = File.createTempFile(file.name, ".tmp", directory)
        try {
            temporary.writeText(json.encodeToString(serializer, machines))
            Files.move(temporary.toPath(), file.toPath(), StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING)
        } finally {
            temporary.delete()
        }
        cache = machines
    }

    companion object {
        private val serializer = ListSerializer(MachineRecord.serializer())
        private val json = Json {
            prettyPrint = true
            ignoreUnknownKeys = true
        }

        /** 默认位置：不参与备份的私有目录下的 `machines.json`。私钥不随备份迁走，记录跟着它留在本机。 */
        fun defaultFile(context: Context): File = File(context.noBackupFilesDir, "machines.json")
    }
}

internal object UUIDSerializer : KSerializer<UUID> {
    override val descriptor = PrimitiveSerialDescriptor("UUID", PrimitiveKind.STRING)

    override fun serialize(encoder: Encoder, value: UUID) = encoder.encodeString(value.toString())

    override fun deserialize(decoder: Decoder): UUID = UUID.fromString(decoder.decodeString())
}

/** ISO 8601 的 UTC 时刻。 */
internal object InstantSerializer : KSerializer<Instant> {
    override val descriptor = PrimitiveSerialDescriptor("Instant", PrimitiveKind.STRING)

    override fun serialize(encoder: Encoder, value: Instant) = encoder.encodeString(value.toString())

    override fun deserialize(decoder: Decoder): Instant = Instant.parse(decoder.decodeString())
}
