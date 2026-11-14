package dev.runode.mobile.features.model

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.runode.mobile.connection.LinkFailure
import dev.runode.mobile.connection.MachineRecord
import dev.runode.mobile.connection.Pairing
import dev.runode.mobile.features.R
import dev.runode.mobile.protocol.PairingInvitation
import dev.runode.mobile.protocol.PairingInvitationError
import java.time.Instant
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * 配对页：扫电脑上的二维码，或者粘贴 `runode://pair?...` 链接（模拟器上没有摄像头时用）。
 *
 * [onPaired] 把配好的电脑存下来，存不下时抛出，配对页显示失败。
 */
class PairingModel(
    private val pairing: Pairing,
    private val deviceName: String,
    private val now: () -> Instant = { Instant.now() },
    private val onPaired: suspend (MachineRecord) -> Unit,
) {
    sealed class Phase {
        data object Idle : Phase()

        /** 正在和这台电脑配对。 */
        data class Pairing(val hostName: String) : Phase()
        data class Paired(val machine: MachineRecord) : Phase()
        data class Failed(val message: UiText) : Phase()
    }

    var linkText by mutableStateOf("")

    /** 扫不了码的原因（没有摄像头、没给权限）；为空时显示取景框。 */
    var scannerUnavailable by mutableStateOf<UiText?>(null)

    var phase by mutableStateOf<Phase>(Phase.Idle)
        private set

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    /** 正在跑的配对；配对页关掉时撤掉。 */
    private var running: Job? = null

    /** 配对页关掉了：之后配好的不再存。 */
    private var cancelled = false

    /** 已经配成的码：回到扫码时镜头里往往还是它，再扫到不重配同一台电脑。 */
    private val pairedCodes = mutableSetOf<String>()

    /** 输入框里的链接解析出来的邀请，配对前给用户看要连的电脑；不是配对链接时为空。 */
    val invitation: PairingInvitation?
        get() = runCatching { PairingInvitation.parse(linkText) }.getOrNull()

    val isBusy: Boolean get() = phase is Phase.Pairing

    /** 用输入框里的链接配对。 */
    suspend fun submitLink() {
        pair(linkText)
    }

    /** 扫到一个二维码。正在配对、已经配好，或者这个码已经配成过时忽略。 */
    suspend fun scanned(code: String) {
        if (phase is Phase.Pairing || phase is Phase.Paired) return
        if (code in pairedCodes) return
        linkText = code
        pair(code)
    }

    fun reset() {
        phase = Phase.Idle
    }

    /**
     * 配对页关掉了：撤掉正在跑的配对，配好了也不存（不换掉同一台电脑的旧配对），为它存的私钥删掉（`Pairing.discard`）。
     * 已经在存的照样存完。
     */
    fun cancel() {
        cancelled = true
        running?.cancel()
    }

    private suspend fun pair(text: String) {
        if (isBusy || cancelled) return
        val invitation = try {
            PairingInvitation.parse(text)
        } catch (e: PairingInvitationError) {
            phase = Phase.Failed(Presentation.pairingInvitationError(e))
            return
        } catch (e: Exception) {
            phase = Phase.Failed(UiText.res(R.string.pairing_link_invalid))
            return
        }
        if (invitation.isExpired(now())) {
            phase = Phase.Failed(Presentation.linkFailure(LinkFailure.InvitationExpired))
            return
        }
        phase = Phase.Pairing(invitation.hostName)
        // 配对连同存下或丢掉结果都跑在模型自己的作用域里，只随 cancel 撤：配对页的组合被销毁（Activity 重建）时，
        // 电脑已经收下的配对照样存下，phase 照样落定。
        val job = scope.launch { finishPairing(text, invitation) }
        running = job
        job.join()
    }

    private suspend fun finishPairing(text: String, invitation: PairingInvitation) {
        try {
            val machine = try {
                pairing.pair(invitation, deviceName)
            } catch (e: CancellationException) {
                throw e
            } catch (e: LinkFailure) {
                if (!cancelled) phase = Phase.Failed(Presentation.linkFailure(e))
                return
            } catch (e: Exception) {
                if (!cancelled) {
                    phase = Phase.Failed(UiText.res(R.string.pairing_failed_detail, e.message ?: e.toString()))
                }
                return
            }
            if (cancelled || !currentCoroutineContext().isActive) {
                pairing.discard(machine)
                return
            }
            try {
                withContext(NonCancellable) { onPaired(machine) }
            } catch (e: Exception) {
                phase = Phase.Failed(UiText.res(R.string.pairing_save_failed, e.message ?: e.toString()))
                return
            }
            pairedCodes += text
            phase = Phase.Paired(machine)
        } finally {
            running = null
        }
    }
}
