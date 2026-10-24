// swift-tools-version: 6.0
// runode iOS 端除 App 入口以外的全部代码。依赖只能自下而上：
//   RunodeProtocol（帧、消息、门禁，纯 Foundation）
//   ← RunodeConnection（TLS、门禁流程、配对、Keychain、Bonjour、重连；跨网络时经 iroh 的 IrohLib）
//   RunodeTerminal（libghostty-vt 的封装和终端 UIView，只依赖 RunodeProtocol 里的数据类型）
//   RunodeSpeech（离线语音识别：模型下载、录音、sherpa-onnx，不依赖别的 target）
//   ← RunodeFeatures（视图模型和 SwiftUI 视图，依赖以上四个）
// 也给 macOS 声明了平台：没有界面的部分（以及视图模型）能直接在 Mac 上 `swift test`，UIKit 的部分
// 用 `#if os(iOS)` 包着。
import PackageDescription

let package = Package(
    name: "RunodeKit",
    defaultLocalization: "zh-Hans",
    platforms: [.iOS(.v18), .macOS(.v15)],
    products: [
        .library(name: "RunodeProtocol", targets: ["RunodeProtocol"]),
        .library(name: "RunodeConnection", targets: ["RunodeConnection"]),
        .library(name: "RunodeTerminal", targets: ["RunodeTerminal"]),
        .library(name: "RunodeSpeech", targets: ["RunodeSpeech"]),
        .library(name: "RunodeFeatures", targets: ["RunodeFeatures"]),
    ],
    dependencies: [
        // iroh 的 Swift 绑定，带预编译的 xcframework。和电脑那边的 iroh 同属 1.x，线上格式稳定。
        .package(url: "https://github.com/n0-computer/iroh-ffi", exact: "1.1.0")
    ],
    targets: [
        // libghostty-vt 的 xcframework，由 iOS 端自带的构建脚本从 ghostty 子模块编出来，不进 git。
        .binaryTarget(name: "GhosttyVt", path: "../Frameworks/ghostty-vt.xcframework"),
        // sherpa-onnx 和它用的 onnxruntime 的 iOS 静态 xcframework，和 sherpa-onnx 官方 SwiftPM 包里的是同一份；
        // 直接按地址取，不用把将近 1 GB 的 sherpa-onnx 仓库整个克隆下来。升级时两个一起换，版本要和官方包
        // 里配的一致。
        .binaryTarget(
            name: "SherpaOnnxC",
            url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/xcframework/sherpa-onnx-v2.0.0-ios-static.xcframework.zip",
            checksum: "95550cc9e2d9699eccbc41d6d7462fb83cdfc12dd070d937533f8d0383a38286"),
        .binaryTarget(
            name: "OnnxRuntime",
            url: "https://github.com/csukuangfj/onnxruntime-libs/releases/download/v1.28.2/onnxruntime-ios-static-xcframework-1.28.2.xcframework.zip",
            checksum: "2c2299acbb461d26d4bac4bc85985d40e7c7177ed6072703ae0846d88b0b4599"),
        .target(name: "RunodeProtocol"),
        .target(
            name: "RunodeConnection",
            dependencies: ["RunodeProtocol", .product(name: "IrohLib", package: "iroh-ffi")],
            // iroh 在苹果平台上用这几个框架看网卡和网络变化。
            linkerSettings: [
                .linkedFramework("SystemConfiguration"), .linkedFramework("Network"),
                .linkedFramework("CoreWLAN", .when(platforms: [.macOS])),
            ]
        ),
        .target(
            name: "RunodeTerminal",
            dependencies: ["RunodeProtocol", "GhosttyVt"],
            // Nerd Fonts 的 Symbols Only 字体和它的许可文件；libghostty-vt 和编进它的 simdutf、highway、uucode
            // 的许可随 app 分发，界面上不显示。highway 是 Apache-2.0 和 BSD-3 二选一，取 BSD-3。
            resources: [.copy("Resources/NerdFontsSymbolsOnly"), .copy("Resources/Licenses")],
            // libghostty-vt 里的 simdutf、highway 是 C++ 写的。
            linkerSettings: [.linkedLibrary("c++")]
        ),
        // 只有 iOS 的识别库，在 Mac 上 `swift test` 时识别报 `SpeechError.unsupported`，下载和录音照常编译。
        .target(
            name: "RunodeSpeech",
            dependencies: [
                .target(name: "SherpaOnnxC", condition: .when(platforms: [.iOS])),
                .target(name: "OnnxRuntime", condition: .when(platforms: [.iOS])),
            ],
            linkerSettings: [.linkedLibrary("c++"), .linkedFramework("CoreML", .when(platforms: [.iOS]))]
        ),
        .target(
            name: "RunodeFeatures",
            dependencies: ["RunodeProtocol", "RunodeConnection", "RunodeTerminal", "RunodeSpeech"],
            // 各家 agent 的 logo：图片是桌面资源里那几个 SVG 的符号链接，出处和许可见桌面那边的 LICENSE。
            resources: [.process("Resources/AgentLogos.xcassets")]
        ),
        .testTarget(name: "RunodeProtocolTests", dependencies: ["RunodeProtocol"]),
        .testTarget(name: "RunodeConnectionTests", dependencies: ["RunodeConnection"]),
        .testTarget(name: "RunodeTerminalTests", dependencies: ["RunodeTerminal"]),
        .testTarget(name: "RunodeSpeechTests", dependencies: ["RunodeSpeech"]),
        .testTarget(name: "RunodeFeaturesTests", dependencies: ["RunodeFeatures"]),
    ]
)
