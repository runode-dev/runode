// swift-tools-version: 6.0
// runode iOS 端除 App 入口以外的全部代码。依赖只能自下而上：
//   RunodeProtocol（帧、消息、门禁，纯 Foundation）
//   ← RunodeConnection（TLS、门禁流程、配对、Keychain、Bonjour、重连；跨网络时经 iroh 的 IrohLib）
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
    dependencies: [
        // iroh 的 Swift 绑定，带预编译的 xcframework。和电脑那边的 iroh 同属 1.x，线上格式稳定。
        .package(url: "https://github.com/n0-computer/iroh-ffi", exact: "1.1.0")
    ],
    targets: [
        // libghostty-vt 的 xcframework，由 iOS 端自带的构建脚本从 ghostty 子模块编出来，不进 git。
        .binaryTarget(name: "GhosttyVt", path: "../Frameworks/ghostty-vt.xcframework"),
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
        .target(
            name: "RunodeFeatures",
            dependencies: ["RunodeProtocol", "RunodeConnection", "RunodeTerminal"],
            // 各家 agent 的 logo：图片是桌面资源里那几个 SVG 的符号链接，出处和许可见桌面那边的 LICENSE。
            resources: [.process("Resources/AgentLogos.xcassets")]
        ),
        .testTarget(name: "RunodeProtocolTests", dependencies: ["RunodeProtocol"]),
        .testTarget(name: "RunodeConnectionTests", dependencies: ["RunodeConnection"]),
        .testTarget(name: "RunodeTerminalTests", dependencies: ["RunodeTerminal"]),
        .testTarget(name: "RunodeFeaturesTests", dependencies: ["RunodeFeatures"]),
    ]
)
