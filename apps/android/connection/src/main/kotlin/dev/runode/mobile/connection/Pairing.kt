package dev.runode.mobile.connection

import dev.runode.mobile.protocol.PairingInvitation
import java.util.UUID
import kotlin.coroutines.cancellation.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** 拿着二维码里的邀请和电脑配对。 */
interface Pairing {
    /** 配对成功时设备私钥已经存好，返回这台电脑的记录（还没存进 [MachineStore]）。 */
    suspend fun pair(invitation: PairingInvitation, deviceName: String): MachineRecord

    /**
     * [pair] 配好了、调用方却不要了（配对页已经关掉）：删掉为它存的设备私钥，不然这把钥匙没有记录认领，一直留在
     * Keystore 里。不存私钥的配对（测试、演示）什么也不做。
     */
    fun discard(machine: MachineRecord) {}
}

/**
 * 真的配对：按 Bonjour 找到的服务和二维码里的地址挨个试，都不通时经 iroh（[tunnel] 不为空、二维码里有 `node` 时）按
 * 二维码里的公钥拨过去，门禁里发 `remote_pair`。口令只能用一次，所以一条一条试、不同时连。配对用的连接在放行后就关掉，
 * 之后按已配对的设备正常登录。设备私钥先按新记录的编号生成在 [keyStore] 里，没配成就删掉。
 */
class RemotePairing(
    private val keyStore: DeviceKeyStore,
    private val discovery: HostDiscovery,
    private val open: TransportOpener = { target, fingerprint -> TlsChannel.open(target, fingerprint) },
    private val tunnel: TunnelOpener? = { node, fingerprint -> IrohDialer.shared.open(node, fingerprint) },
) : Pairing {
    override suspend fun pair(invitation: PairingInvitation, deviceName: String): MachineRecord {
        if (invitation.isExpired()) throw LinkFailure.InvitationExpired
        val id = UUID.randomUUID()
        // Keystore 生成密钥可能要走一趟安全芯片，不占着调用方的线程。
        val key = withContext(Dispatchers.IO) { keyStore.generate(id) }
        try {
            return pair(invitation, deviceName, id, key)
        } catch (error: Throwable) {
            runCatching { keyStore.deleteKey(id) }
            throw error
        }
    }

    private suspend fun pair(invitation: PairingInvitation, deviceName: String, id: UUID, key: DeviceKey): MachineRecord {
        val targets = discovery.targets(
            invitation.fingerprint,
            invitation.addresses.mapNotNull { TransportTarget.address(it, invitation.port) },
        )
        suspend fun attempt(transport: FrameTransport): MachineRecord {
            try {
                val outcome = GateClient.run(transport, GateCredential.Pair(invitation.secret, deviceName, key))
                return MachineRecord(
                    id = id,
                    name = invitation.hostName,
                    hostName = outcome.hostName.ifEmpty { invitation.hostName },
                    fingerprint = invitation.fingerprint,
                    port = invitation.port,
                    addresses = invitation.addresses,
                    lastAddress = transport.remoteAddress,
                    deviceId = outcome.deviceId,
                    node = outcome.node ?: invitation.node,
                )
            } finally {
                transport.close()
            }
        }
        return try {
            firstSuccess(targets) { target -> attempt(open(target, invitation.fingerprint)) }
        } catch (failure: LinkFailure) {
            if (failure.isFatal) throw failure
            fallBackToTunnel(invitation, failure, ::attempt)
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            fallBackToTunnel(invitation, error, ::attempt)
        }
    }

    private suspend fun fallBackToTunnel(
        invitation: PairingInvitation,
        error: Exception,
        attempt: suspend (FrameTransport) -> MachineRecord,
    ): MachineRecord {
        val node = invitation.node
        if (tunnel == null || node == null) throw error
        return attempt(tunnel(node, invitation.fingerprint))
    }

    override fun discard(machine: MachineRecord) {
        runCatching { keyStore.deleteKey(machine.id) }
    }
}
