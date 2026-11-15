# Runode Privacy Policy

[简体中文](privacy.zh-CN.md)

Last updated: November 16, 2026

Runode is a terminal for AI coding agents on your Mac, with an iOS app that controls it remotely. This policy covers the Runode Mac app and the Runode iOS app.

## We don't collect your data

Runode has no accounts, no analytics, no advertising and no tracking. We don't run servers that store your terminals, files, voice or any other personal data. Everything the apps show comes from your own Mac.

## Where your data goes

- **Between your phone and your Mac.** The iOS app talks only to Macs you have paired by scanning a QR code. The connection is end-to-end encrypted with TLS 1.3 and pinned to your Mac's certificate. On the same network it goes directly to your Mac. Elsewhere, only if you allow connections over the internet (network 0.0.0.0 when pairing), it is relayed through public relay servers run by the iroh project (n0); those relays only see encrypted traffic and cannot read it.
- **Notifications.** To notify your phone when an agent needs you, your Mac sends the notification through our push relay (push.runode.dev) to Apple Push Notification service for an iPhone, or to Google's Firebase Cloud Messaging (FCM) for an Android phone. Your phone gives its push token only to the relay, which hands back a sealed handle for each Mac; your Mac only ever sees that handle. The relay forwards notifications to Apple or Google in real time and does not store them. On Android the notification text travels through FCM as message data, and the app shows it itself.
- **Notifications from apps you build yourself.** A phone app you build from source without the relay gives its push token directly to the Macs it is paired with, which keep it in their list of paired devices. Your Mac then sends notifications straight to Apple Push Notification service with the APNs key you configured, or to FCM with the Firebase service account you configured, without going through our relay. Those notifications are handled under your own Apple or Google developer account, under their terms.
- **Voice input.** Speech is recognized on your phone with an offline model, or on your Mac. Recordings are not sent to us. If you set up a cloud speech or text service in the Mac app, audio or text is sent to the provider you configured, under that provider's terms.
- **Speech model downloads.** The offline speech model is downloaded from Hugging Face or ModelScope. Those sites see an ordinary download request.
- **Updates.** The Mac app checks GitHub Releases for new versions and downloads them from GitHub.
- **Stored on your devices.** Paired Macs, settings, and a Mac login password you choose to save (kept in the iOS Keychain and unlocked with Face ID, or on Android encrypted with a key in the Android Keystore and unlocked with your fingerprint or face) stay on your devices.

## Permissions

- **Camera**: only to scan the pairing QR code.
- **Microphone**: only while you hold the microphone key for voice input.
- **Local network**: to find and connect to your Mac.
- **Face ID**: to unlock a saved Mac login password.

## Children

Runode is a developer tool and is not directed at children.

## Changes and contact

Changes to this policy are published at this address. For questions, open an issue at https://github.com/runode-dev/runode/issues.
