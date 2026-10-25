# Runode Privacy Policy

Last updated: October 24, 2026

Runode is a terminal for AI coding agents on your Mac, with an iOS app that controls it remotely. This policy covers the Runode Mac app and the Runode iOS app.

## We don't collect your data

Runode has no accounts, no analytics, no advertising and no tracking. We don't run servers that store your terminals, files, voice or any other personal data. Everything the apps show comes from your own Mac.

## Where your data goes

- **Between your phone and your Mac.** The iOS app talks only to Macs you have paired by scanning a QR code. The connection is end-to-end encrypted with TLS 1.3 and pinned to your Mac's certificate. On the same network it goes directly to your Mac. Elsewhere it is relayed through public relay servers run by the iroh project (n0); those relays only see encrypted traffic and cannot read it.
- **Notifications.** To notify your phone when an agent needs you, your Mac sends the notification to Apple Push Notification service through our push relay (push.runode.dev). Your phone gives its device token only to the relay, which hands back a sealed handle for each Mac; your Mac only ever sees that handle. The relay forwards notifications to Apple in real time and does not store them.
- **Voice input.** Speech is recognized on your phone with an offline model, or on your Mac. Recordings are not sent to us. If you set up a cloud speech or text service in the Mac app, audio or text is sent to the provider you configured, under that provider's terms.
- **Speech model downloads.** The offline speech model is downloaded from Hugging Face or ModelScope. Those sites see an ordinary download request.
- **Updates.** The Mac app checks GitHub Releases for new versions and downloads them from GitHub.
- **Stored on your devices.** Paired Macs, settings, and a Mac login password you choose to save (kept in the iOS Keychain and unlocked with Face ID) stay on your devices.

## Permissions

- **Camera**: only to scan the pairing QR code.
- **Microphone**: only while you hold the microphone key for voice input.
- **Local network**: to find and connect to your Mac.
- **Face ID**: to unlock a saved Mac login password.

## Children

Runode is a developer tool and is not directed at children.

## Changes and contact

Changes to this policy are published at this address. For questions, open an issue at https://github.com/runode-dev/runode/issues.

---

# Runode 隐私政策

最后更新：2026 年 10 月 24 日

Runode 是 Mac 上为 AI 编程 agent 做的终端，另有一个 iOS app 用来远程操作它。本政策适用于 Runode 的 Mac app 和 iOS app。

## 我们不收集你的数据

Runode 没有账号，不做统计分析，没有广告，也不追踪你。我们没有任何服务器存你的终端、文件、语音或其他个人数据，app 里显示的一切都来自你自己的 Mac。

## 你的数据去了哪里

- **手机和 Mac 之间。** iOS app 只和你扫码配对过的 Mac 通信，连接用 TLS 1.3 端到端加密，并绑定那台 Mac 的证书。在同一个网络里直接连到 Mac；不在同一个网络时经 iroh 项目（n0）运营的公共中继转发，中继只经手加密后的数据，读不到内容。
- **通知。** agent 需要你时，Mac 经我们的推送中转（push.runode.dev）把通知交给 Apple 推送通知服务。手机只把设备令牌交给中转，换回每台 Mac 各一个加密的句柄，Mac 只拿得到句柄。中转实时把通知转给 Apple，不保存。
- **语音输入。** 语音在手机上用离线模型识别，或交给你的 Mac 识别，录音不会发给我们。如果你在 Mac app 里配置了云端识别或文字润色服务，音频或文字会发给你配置的服务商，适用该服务商的条款。
- **语音模型下载。** 离线语音模型从 Hugging Face 或 ModelScope 下载，这些网站看到的是一次普通的下载请求。
- **更新。** Mac app 向 GitHub Releases 查询新版本，并从 GitHub 下载。
- **存在你设备上的。** 配对过的 Mac、各项设置，以及你选择保存的 Mac 登录密码（存在 iOS 钥匙串里，用面容 ID 取出）都只留在你的设备上。

## 权限

- **相机**：只用来扫配对二维码。
- **麦克风**：只在你按住麦克风键语音输入时使用。
- **本地网络**：找到并连接你的 Mac。
- **面容 ID**：取出保存的 Mac 登录密码。

## 儿童

Runode 是开发者工具，不面向儿童。

## 变更和联系方式

本政策的变更会发布在这个地址。有问题请在 https://github.com/runode-dev/runode/issues 提 issue。
