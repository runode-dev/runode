#if os(iOS)
    import RunodeConnection
    import RunodeProtocol
    import RunodeTerminal
    import SwiftUI

    /// 一台 Mac 上的会话。点进去开终端，左滑结束会话，右上角新开一个。
    struct SessionListView: View {
        @Bindable var model: SessionListModel
        let onOpen: (SessionId) -> Void
        @Environment(\.displayScale) private var displayScale

        var body: some View {
            List {
                if !model.linkState.isConnected {
                    Section {
                        ConnectionStatusRow(state: model.linkState, onRetry: model.reconnect)
                    }
                }
                Section {
                    ForEach(model.sessions, id: \.id) { session in
                        Button {
                            onOpen(session.id)
                        } label: {
                            SessionRow(session: session)
                        }
                        .tint(.primary)
                        .swipeActions(edge: .trailing) {
                            Button("结束", systemImage: "xmark.circle", role: .destructive) {
                                model.killTarget = session.id
                            }
                        }
                    }
                } footer: {
                    if model.loaded, !model.sessions.isEmpty {
                        Text("左滑可以结束会话。")
                    }
                }
            }
            .overlay {
                if model.loaded, model.sessions.isEmpty, model.linkState.isConnected {
                    ContentUnavailableView {
                        Label("没有终端", systemImage: "terminal")
                    } actions: {
                        Button("新开一个") { Task { await model.spawn() } }
                            .buttonStyle(.borderedProminent)
                    }
                }
            }
            .navigationTitle(model.machine.name)
            .toolbar {
                ToolbarItem(placement: .primaryAction) {
                    Button("新终端", systemImage: "plus") {
                        Task { await model.spawn() }
                    }
                    .disabled(!model.linkState.isConnected || model.isSpawning)
                }
            }
            .refreshable { model.refresh() }
            .task { await model.keepRefreshing() }
            .onGeometryChange(for: CGSize.self) { $0.size } action: { size in
                model.spawnSize = TerminalView.gridSize(fitting: size, scale: displayScale)
            }
            .onChange(of: model.spawnedSession) { _, session in
                guard let session else { return }
                model.spawnedSession = nil
                onOpen(session)
            }
            .confirmationDialog(
                "结束这个终端？", isPresented: $model.isConfirmingKill, titleVisibility: .visible,
                presenting: model.killTarget.flatMap(model.session)
            ) { session in
                Button("结束", role: .destructive) { model.kill(session.id) }
            } message: { session in
                Text("「\(Presentation.sessionTitle(session))」里正在跑的程序会收到 SIGHUP 并退出。")
            }
            .alert(
                "出错了", isPresented: Binding(get: { model.errorMessage != nil }, set: { if !$0 { model.errorMessage = nil } })
            ) {
                Button("好") {}
            } message: {
                Text(model.errorMessage ?? "")
            }
        }
    }

    struct ConnectionStatusRow: View {
        let state: LinkState
        let onRetry: () -> Void

        var body: some View {
            HStack(spacing: 10) {
                switch state {
                case .connecting, .idle:
                    ProgressView()
                case .waiting:
                    Image(systemName: "wifi.exclamationmark").foregroundStyle(.orange)
                case .failed:
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red)
                case .connected:
                    Image(systemName: "checkmark.circle.fill").foregroundStyle(.green)
                }
                Text(Presentation.linkState(state))
                    .font(.subheadline)
                Spacer()
                switch state {
                case .waiting, .failed:
                    Button("重试", action: onRetry)
                        .buttonStyle(.bordered)
                default:
                    EmptyView()
                }
            }
        }
    }

    private struct SessionRow: View {
        let session: SessionInfo

        var body: some View {
            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text(Presentation.sessionTitle(session))
                        .font(.headline)
                        .lineLimit(1)
                    if session.exited {
                        Text("已退出")
                            .font(.caption)
                            .padding(.horizontal, 6)
                            .background(.quaternary, in: Capsule())
                    }
                    Spacer()
                    if let status = Presentation.agentStatus(session.meta.agent) {
                        AgentBadge(text: status.text, state: status.state)
                    }
                }
                if let directory = Presentation.directory(session.meta.cwd) {
                    Text(directory)
                        .font(.subheadline.monospaced())
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.head)
                }
                Text("\(Presentation.gridSize(session.size)) · \(Presentation.sizeOwner(session.sizeOwner))")
                    .font(.caption)
                    .foregroundStyle(.tertiary)
            }
            .contentShape(Rectangle())
            .padding(.vertical, 2)
        }
    }

    private struct AgentBadge: View {
        let text: String
        let state: AgentState

        var body: some View {
            Text(text)
                .font(.caption.weight(.medium))
                .padding(.horizontal, 8)
                .padding(.vertical, 3)
                .foregroundStyle(color)
                .background(color.opacity(0.15), in: Capsule())
        }

        private var color: Color {
            switch state {
            case .working: .blue
            case .blocked: .orange
            default: .secondary
            }
        }
    }
#endif
