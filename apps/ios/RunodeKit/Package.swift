// swift-tools-version: 6.0
// runode iOS 端除 App 入口以外的全部代码。依赖只能自下而上：
//   RunodeProtocol（帧、消息、门禁，纯 Foundation）
//   ← RunodeConnection（TLS、门禁流程、配对、Keychain、Bonjour、重连）
//   RunodeTerminal（libghostty-vt 的封装和终端 UIView，只依赖 RunodeProtocol 里的数据类型）
//   ← RunodeFeatures（视图模型和 SwiftUI 视图，依赖以上三个）
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
        .library(name: "RunodeFeatures", targets: ["RunodeFeatures"]),
    ],
    targets: [
        // libghostty-vt 的 xcframework，由 apps/ios 自带的构建脚本从 ghostty 子模块编出来，不进 git。
        .binaryTarget(name: "GhosttyVt", path: "../Frameworks/ghostty-vt.xcframework"),
        .target(name: "RunodeProtocol"),
        .target(name: "RunodeConnection", dependencies: ["RunodeProtocol"]),
        .target(
            name: "RunodeTerminal",
            dependencies: ["RunodeProtocol", "GhosttyVt"],
            // Nerd Fonts 的 Symbols Only 字体和它的许可文件。
            resources: [.copy("Resources/NerdFontsSymbolsOnly")],
            // libghostty-vt 里的 simdutf、highway 是 C++ 写的。
            linkerSettings: [.linkedLibrary("c++")]
        ),
        .target(name: "RunodeFeatures", dependencies: ["RunodeProtocol", "RunodeConnection", "RunodeTerminal"]),
        .testTarget(name: "RunodeProtocolTests", dependencies: ["RunodeProtocol"], resources: [.copy("Fixtures")]),
        .testTarget(name: "RunodeConnectionTests", dependencies: ["RunodeConnection"]),
        .testTarget(name: "RunodeTerminalTests", dependencies: ["RunodeTerminal"]),
        .testTarget(name: "RunodeFeaturesTests", dependencies: ["RunodeFeatures"]),
    ]
)
