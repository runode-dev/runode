#if os(iOS)
    import RunodeProtocol
    import SwiftUI

    extension PairingModel: Identifiable {}

    /// App 的根视图：配对过的 Mac 列表，往里是会话列表和终端页。
    public struct RootView: View {
        @Bindable var app: AppModel
        @Environment(\.scenePhase) private var scenePhase

        public init(app: AppModel) {
            self.app = app
        }

        public var body: some View {
            NavigationStack(path: $app.path) {
                MachineListView(
                    model: app.machineList, onPair: { app.startPairing() },
                    onOpen: { app.path.append(.machine($0)) }
                )
                .navigationDestination(for: Route.self) { route in
                    destination(for: route)
                }
            }
            .sheet(item: $app.pairing) { model in
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
        }

        @ViewBuilder
        private func destination(for route: Route) -> some View {
            switch route {
            case .machine(let id):
                if let model = app.sessionList(for: id) {
                    SessionListView(model: model) { session in
                        app.path.append(.terminal(machine: id, session: session))
                    }
                } else {
                    ContentUnavailableView("找不到这台 Mac", systemImage: "desktopcomputer.trianglebadge.exclamationmark")
                }
            case .terminal(let machine, let session):
                if let model = app.terminal(machine: machine, session: session) {
                    TerminalScreen(model: model)
                } else {
                    ContentUnavailableView("找不到这个终端", systemImage: "terminal")
                }
            }
        }
    }
#endif
