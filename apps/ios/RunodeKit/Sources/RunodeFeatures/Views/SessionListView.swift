#if os(iOS)
    import RunodeConnection
    import RunodeProtocol
    import RunodeTerminal
    import SwiftUI
    import UIKit

    /// 一台电脑上的会话，按电脑上的 app 里的工作区分节，节头能在那个工作区里新开终端；不在任何窗口里的
    /// 会话放在最后的「后台」一节。一个会话一张卡片，带 agent 状态和屏幕最后几行的预览，等你回答的带
    /// 快速回复。点开终端，左滑结束，长按有更多操作（含会话目录里 Makefile、package.json 的命令）。右上角
    /// 能新开终端、新建工作区。
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
                        WorkspaceHeader(section: section, isSpawning: model.isSpawning) { anchor in
                            Task { await model.spawn(near: anchor) }
                        }
                        ForEach(section.sessions, id: \.id) { session in
                            row(session, group: SessionGroup.of(session))
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
                    Menu("新建", systemImage: "plus") {
                        Button("新开终端", systemImage: "terminal") {
                            Task { await model.spawn() }
                        }
                        Button("新建工作区", systemImage: "folder.badge.plus") {
                            model.beginNewWorkspace()
                        }
                    }
                    .disabled(!model.linkState.isConnected || model.isSpawning)
                }
            }
            .sheet(
                isPresented: Binding(
                    get: { model.directoryPicker != nil }, set: { if !$0 { model.cancelNewWorkspace() } })
            ) {
                if let picker = model.directoryPicker {
                    DirectoryPickerView(
                        picker: picker, hasDesktopWindow: model.hasDesktopWindow, onCancel: model.cancelNewWorkspace
                    ) { dir in
                        Task { await model.createWorkspace(at: dir) }
                    }
                }
            }
            .refreshable {
                model.refresh()
                model.refreshPreviews()
                await model.refreshProjectTasks()
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
            let summary = SessionRow(session: session, preview: model.previews[session.id] ?? [])
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
                    ProjectTasksSection(model: model, session: session) { onOpen(session.id) }
                    Section(
                        "\(Presentation.gridSize(session.size)) · \(Presentation.sizeOwner(session.sizeOwner))"
                    ) {
                        Button("结束会话", systemImage: "xmark.circle", role: .destructive) {
                            model.killTarget = session.id
                        }
                    }
                }
                .task(id: session.meta.cwd) {
                    if let cwd = session.meta.cwd { await model.loadProjectTasks(in: cwd) }
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

    /// 菜单里会话目录的项目命令：Makefile、package.json 各一个子菜单，点一条就跑（见
    /// `SessionListModel.runProjectTask`），在这个会话里跑了时调 `onRun`，在新终端里跑时由会话列表打开
    /// 新终端。终端结束了时子菜单点不开。会话卡片的长按菜单和终端页的「⋯」菜单都用它。
    struct ProjectTasksSection: View {
        let model: SessionListModel
        let session: SessionInfo
        var onRun: () -> Void = {}

        var body: some View {
            let sources = model.projectTasks(for: session).filter { !$0.tasks.isEmpty }
            if !sources.isEmpty {
                let runnable = model.canRunProjectTask(in: session)
                Section(Presentation.projectTasksHeader(session)) {
                    ForEach(sources, id: \.file) { source in
                        Menu {
                            ForEach(source.tasks, id: \.name) { task in
                                Button {
                                    Task {
                                        if await model.runProjectTask(task, in: session.id) { onRun() }
                                    }
                                } label: {
                                    Text(task.name)
                                    if let description = task.description { Text(description) }
                                }
                            }
                        } label: {
                            Label(
                                Presentation.taskSourceTitle(source, cwd: session.meta.cwd),
                                systemImage: Presentation.taskSourceSymbol(source))
                        }
                        .disabled(!runnable)
                    }
                }
            }
        }
    }

    /// 一节的标题：工作区的名字和目录，右边一个在这个工作区里新开终端的按钮；后台那一节只有标题。
    private struct WorkspaceHeader: View {
        let section: SessionSection
        let isSpawning: Bool
        let onSpawn: (SessionId) -> Void

        var body: some View {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    Label(Presentation.sectionTitle(section), systemImage: Presentation.sectionSymbol(section))
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(.primary)
                        .accessibilityAddTraits(.isHeader)
                    if let detail = Presentation.sectionDetail(section) {
                        Text(detail)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(1)
                            .truncationMode(.head)
                    }
                }
                Spacer(minLength: 8)
                if let anchor = section.anchor {
                    Button("在这里新开终端", systemImage: "plus") { onSpawn(anchor) }
                        .labelStyle(.iconOnly)
                        .font(.body.weight(.semibold))
                        .buttonStyle(.borderless)
                        .disabled(isSpawning)
                        .accessibilityLabel("在「\(Presentation.sectionTitle(section))」里新开终端")
                }
            }
            .padding(.horizontal, 16)
            .padding(.top, 12)
            .plainListRow()
        }
    }

    private struct SessionRow: View {
        let session: SessionInfo
        let preview: [String]

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
                    AgentBadge(agent: session.meta.agent)
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
