#if DEBUG
    import Foundation
    import RunodeFeatures
    import RunodeProtocol

    /// 只在调试构建里有的启动参数，给模拟器上的端到端联调用：模拟器里没法点系统的「在 Runode 中
    /// 打开」确认框，也不方便点列表，就由启动参数直接走到要看的那一步。
    /// - `-runode-link <runode://pair?...>`：拿这条配对链接直接配对，和从别处打开链接一样。
    /// - `-runode-open machine`：打开第一台电脑的会话列表。
    /// - `-runode-open <会话标识>`：打开第一台电脑上这个会话的终端页。
    @MainActor
    enum DebugLaunch {
        static func apply(_ app: AppModel) {
            let arguments = ProcessInfo.processInfo.arguments
            if let link = value(after: "-runode-link", in: arguments) {
                app.startPairing(link: link)
            }
            guard let target = value(after: "-runode-open", in: arguments),
                let machine = app.machineList.machines.first?.id
            else { return }
            if let session = SessionId(target) {
                app.path = [.machine(machine), .terminal(machine: machine, session: session)]
            } else {
                app.path = [.machine(machine)]
            }
        }

        private static func value(after flag: String, in arguments: [String]) -> String? {
            guard let index = arguments.firstIndex(of: flag), arguments.indices.contains(index + 1) else { return nil }
            return arguments[index + 1]
        }
    }
#endif
