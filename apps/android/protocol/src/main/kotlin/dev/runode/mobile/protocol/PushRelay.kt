package dev.runode.mobile.protocol

import kotlinx.serialization.EncodeDefault
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.Serializable

/**
 * 官方 App 为一台电脑向中转服务换推送句柄的请求体，线上格式以中转服务的 `runode_protocol::push::RelayRegister`
 * 为准。电脑只拿得到句柄，拿不到推送的 token。属性的先后就是写出来的键的先后，按线上格式排，不能调换。
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
        /** 要等于中转服务的 `runode_protocol::push::RELAY_REGISTER`，否则中转服务没有这个接口，换不到句柄。 */
        const val path: String = "/v1/register"

        /**
         * 只有这个 App 的推送经中转服务发。要等于 `runode_protocol::push::OFFICIAL_BUNDLE`，否则电脑不把这个
         * App 的推送交给中转服务。
         */
        const val officialBundle: String = "dev.runode.mobile"
    }
}

/** 中转服务换句柄的回话，线上格式以中转服务的 `runode_protocol::push::RelayHandle` 为准。 */
@Serializable
data class RelayHandle(val handle: String)

/** 中转服务出错时的响应体，线上格式以中转服务的 `runode_protocol::push::PushError` 为准。 */
@Serializable
data class PushError(val reason: String)
