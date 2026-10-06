#if os(iOS)
    import RunodeConnection
    import RunodeProtocol
    import RunodeTerminal
    import SwiftUI
    import UIKit

    /// 一台电脑上的会话，以 agent 为中心：等你回答的放最上面（带快速回复），接着是干活中的，最后是
    /// 其他。一个会话一张卡片，带屏幕最后几行的预览。点开终端，左滑结束，长按有更多操作。
    struct SessionListView: View {
        @Bindable var model: SessionListModel
        let onOpen: (SessionId) -> Void
        /// 打开会话所在仓库的 Git 页。
        var onOpenGit: (SessionId) -> Void = { _ in }
        @Environment(\.displayScale) private var displayScale
        @Environment(\.dynamicTypeSize) private var dynamicTypeSize

        var body: some View {
            List {
                if !model.linkState.isConnected {
                    Section {
                        ConnectionStatusRow(state: model.linkState, onRetry: model.reconnect)
                            .cardBackground()
                            .plainListRow()
                    }
                }
                ForEach(model.sections) { section in
                    Section {
                        ListSectionHeader(
                            title: Presentation.title(for: section.group),
                            systemImage: Presentation.symbol(for: section.group),
                            tint: section.group == .other ? Color.secondary : AgentBadge.tint(for: section.group))
                        ForEach(section.sessions, id: \.id) { session in
                            row(session, group: section.group)
                        }
                    }
                }
            }
            .cardList()
            .animation(.default, value: model.sections.map(\.sessions.count))
            .overlay {
                if model.loaded, model.sessions.isEmpty, model.linkState.isConnected {
                    ContentUnavailableView {
                        Label("这台电脑上没有终端", systemImage: "terminal")
                    } description: {
                        Text("新开一个终端，在手机上就能用。")
                    } actions: {
                        Button("新开会话") { Task { await model.spawn() } }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.large)
                    }
                } else if !model.loaded, model.linkState.isConnected {
                    ProgressView("正在读取会话…")
                }
            }
            .leadingNavigationTitle(model.machine.name, subtitle: linkSubtitle)
            .toolbar {
                ToolbarItem(placement: .primaryAction) {
                    Button("新开会话", systemImage: "plus") {
                        Task { await model.spawn() }
                    }
                    .disabled(!model.linkState.isConnected || model.isSpawning)
                }
            }
            .refreshable {
                model.refresh()
                model.refreshPreviews()
            }
            .task { await model.keepRefreshing() }
            .onGeometryChange(for: CGSize.self) { $0.size } action: { size in
                model.spawnSize = TerminalView.gridSize(
                    fitting: size, scale: displayScale, contentSize: UIContentSizeCategory(dynamicTypeSize))
            }
            .alert(
                "出错了", isPresented: Binding(get: { model.errorMessage != nil }, set: { if !$0 { model.errorMessage = nil } })
            ) {
                Button("好") {}
            } message: {
                Text(model.errorMessage ?? "")
            }
        }

        /// 一个会话一张卡片，整张能点开终端。等回答的会话卡片下半截是快速回复，只有上半截能点开。
        @ViewBuilder
        private func card(_ session: SessionInfo, group: SessionGroup) -> some View {
            let summary = SessionRow(session: session, preview: model.previews[session.id] ?? [], group: group)
            if group == .waiting {
                VStack(alignment: .leading, spacing: 10) {
                    Button { onOpen(session.id) } label: { summary.contentShape(Rectangle()) }
                        .buttonStyle(.plain)
                    QuickReplyBar(model: model.quickReply(for: session.id))
                        .accessibilityElement(children: .contain)
                        .accessibilityLabel("回复「\(Presentation.sessionTitle(session))」")
                }
                .cardBackground()
            } else {
                Button { onOpen(session.id) } label: { summary }
                    .buttonStyle(CardButtonStyle())
            }
        }

        /// 标题下面一行连接状态：连着时带地址。
        private var linkSubtitle: String {
            switch model.linkState {
            case .connected(_, let address?): "已连接 · \(Presentation.displayAddress(address))"
            case .waiting: "已断开，正在重连"
            default: Presentation.linkStatus(model.linkState)
            }
        }

        private func row(_ session: SessionInfo, group: SessionGroup) -> some View {
            card(session, group: group)
                .plainListRow()
                .swipeActions(edge: .trailing) {
                    // 不用 `.destructive`：那样系统当作这一行被删了，先把它动画移走，挂在上面的确认框
                    // 跟着消失。只要红色。
                    Button("结束", systemImage: "xmark.circle") {
                        model.killTarget = session.id
                    }
                    .tint(.red)
                }
                .contextMenu {
                    Button("打开", systemImage: "terminal") { onOpen(session.id) }
                    if let cwd = session.meta.cwd {
                        Button("Git", systemImage: "arrow.triangle.branch") { onOpenGit(session.id) }
                        Button("复制目录", systemImage: "doc.on.doc") { UIPasteboard.general.string = cwd }
                    }
                    Section(
                        "\(Presentation.gridSize(session.size)) · \(Presentation.sizeOwner(session.sizeOwner))"
                    ) {
                        Button("结束会话", systemImage: "xmark.circle", role: .destructive) {
                            model.killTarget = session.id
                        }
                    }
                }
                // 挂在卡片上：iOS 26 起确认框是指向所挂视图的气泡，挂在整个列表上会指到屏幕中间。
                .confirmationDialog(
                    "结束这个终端？", isPresented: confirmsKill(session.id), titleVisibility: .visible
                ) {
                    Button("结束会话", role: .destructive) { model.kill(session.id) }
                } message: {
                    Text("「\(Presentation.sessionTitle(session))」里正在跑的程序会收到 SIGHUP 并退出。")
                }
        }

        /// 这个会话是不是等着确认结束。
        private func confirmsKill(_ id: SessionId) -> Binding<Bool> {
            Binding(get: { model.killTarget == id }, set: { if !$0, model.killTarget == id { model.killTarget = nil } })
        }
    }

    private struct SessionRow: View {
        let session: SessionInfo
        let preview: [String]
        let group: SessionGroup

        var body: some View {
            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(Presentation.sessionTitle(session))
                        .font(.headline)
                        .lineLimit(2)
                    if session.exited {
                        Text("已退出")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .padding(.horizontal, 6)
                            .background(.quaternary, in: Capsule())
                    }
                    Spacer(minLength: 8)
                    DisclosureChevron()
                }
                if session.meta.agent != nil {
                    // 等回答、干活中的分组标题已经写了状态，徽标上只写 agent 的名字。
                    AgentBadge(agent: session.meta.agent, showsState: group == .other)
                }
                if let directory = Presentation.directory(session.meta.cwd) {
                    CompactLabel(text: directory, systemImage: "folder")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.head)
                }
                if !preview.isEmpty {
                    ScreenPreview(lines: preview)
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityHint("打开这个终端")
        }
    }
#endif
