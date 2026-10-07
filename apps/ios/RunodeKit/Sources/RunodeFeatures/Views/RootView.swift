#if os(iOS)
    import RunodeProtocol
    import SwiftUI

    extension PairingModel: Identifiable {}

    /// App 的根视图：首页，往里是一台电脑的会话列表和终端页。
    public struct RootView: View {
        @Bindable var app: AppModel
        @Environment(\.scenePhase) private var scenePhase

        public init(app: AppModel) {
            self.app = app
        }

        public var body: some View {
            NavigationStack(path: $app.path) {
                HomeView(app: app, machines: app.machineList)
                .navigationDestination(for: Route.self) { route in
                    destination(for: route)
                }
            }
            .sheet(isPresented: $app.showingSettings, onDismiss: app.settingsDismissed) {
                SettingsView(app: app, settings: app.settings, machines: app.machineList)
                    .appTheme(app.theme)
            }
            .fullScreenCover(item: $app.pairing) { model in
                PairingView(model: model) { machine in
                    app.pairing = nil
                    if let machine {
                        app.path = [.machine(machine.id)]
                    }
                }
            }
            .onChange(of: scenePhase) { _, phase in
                app.setActive(phase != .background)
            }
            .onOpenURL { url in
                guard url.scheme?.lowercased() == "runode" else { return }
                app.startPairing(link: url.absoluteString)
            }
            // 终端页以外的界面跟电脑上终端的主题走；终端页自己按它那个会话的主题上色。弹出的页面各自
            // 再套一次，深浅模式才跟着变。
            .appTheme(app.theme)
        }

        @ViewBuilder
        private func destination(for route: Route) -> some View {
            switch route {
            case .machine(let id):
                if let model = app.sessionList(for: id) {
                    SessionListView(
                        model: model,
                        onOpen: { session in app.openTerminal(machine: id, session: session) },
                        onOpenGit: { session in app.openGit(machine: id, session: session) })
                } else {
                    ContentUnavailableView("找不到这台电脑", systemImage: "desktopcomputer.trianglebadge.exclamationmark")
                }
            case .terminal(let machine, let session):
                if let model = app.terminal(machine: machine, session: session) {
                    TerminalScreen(
                        model: model, preferences: app.settings.preferences,
                        onOpenGit: { app.openGit(machine: machine, session: session) },
                        sessions: app.sessionList(for: machine))
                    // 从终端页开新终端时 `openTerminal` 把栈顶原地换成新会话，同一位置同一类型的页面
                    // SwiftUI 会接着用，嵌着的终端视图不重建、仍挂在旧会话上，新会话没地方画。按会话
                    // 区分身份，换会话就换一整页。
                    .id(route)
                } else {
                    ContentUnavailableView("找不到这个终端", systemImage: "terminal")
                }
            case .git(let machine, let session):
                if let model = app.git(machine: machine, session: session) {
                    GitScreen(model: model)
                } else {
                    ContentUnavailableView("找不到这个终端", systemImage: "terminal")
                }
            }
        }
    }
#endif
