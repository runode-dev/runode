package dev.runode.mobile.protocol

import kotlinx.serialization.EncodeDefault
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.Serializable

/**
 * 官方 App 为一台电脑向中转服务换推送句柄的请求体（宿主的 `push::RelayRegister`）。电脑只拿得到句柄，
 * 拿不到推送的 token。属性的先后就是写出来的键的先后，和宿主写的一样。
 */
@OptIn(ExperimentalSerializationApi::class)
@Serializable
data class RelayRegister(
    val v: UInt = 1u,
    /** APNs 的环境；FCM 时填 [ApnsEnv.Production]，中转不看。 */
    val env: ApnsEnv,
    /** APNs 的 device token（十六进制），或者 FCM 的 registration token 原文。 */
    val token: String,
    /** 手机给这台电脑编的 UUID（[ClientMsg.PushRegister] 的 `machine`）。 */
    val machine: String,
    /** 推送经哪家送到手机；是 [PushService.Apns] 时不写出来。 */
    @EncodeDefault(EncodeDefault.Mode.NEVER) val service: PushService = PushService.Apns,
) {
    companion object {
        /** 宿主的 `push::RELAY_REGISTER`。 */
        const val path: String = "/v1/register"

        /** 宿主的 `push::OFFICIAL_BUNDLE`：只有这个 App 的推送经中转服务发。 */
        const val officialBundle: String = "dev.runode.mobile"
    }
}

/** 中转服务换句柄的回话（宿主的 `push::RelayHandle`）。 */
@Serializable
data class RelayHandle(val handle: String)

/** 中转服务出错时的响应体（宿主的 `push::PushError`）。 */
@Serializable
data class PushError(val reason: String)
