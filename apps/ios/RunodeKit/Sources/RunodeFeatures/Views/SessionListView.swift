#if os(iOS)
    import RunodeConnection
    import RunodeProtocol
    import RunodeTerminal
    import SwiftUI
    import UIKit

    /// 一台电脑上的会话，按电脑上的 app 里的工作区分节，节头能在那个工作区里新开终端，长按能给工作区
    /// 改名；一个终端都没有的工作区也列出来；不在任何窗口里的会话放在最后的「后台」一节。一个会话一张
    /// 卡片，带 agent 状态和屏幕最后几行的预览，等你回答的带快速回复。点开终端，左滑结束，长按有更多操作
    /// （含会话目录里 Makefile、package.json 的命令）。有会话等你回答时，列表顶上汇总一张卡片，点一行滚到
    /// 那个会话。右上角能新开终端、新建工作区。
    struct SessionListView: View {
        @Bindable var model: SessionListModel
        let onOpen: (SessionId) -> Void
        /// 打开会话所在仓库的 Git 页。
        var onOpenGit: (SessionId) -> Void = { _ in }
        @Environment(\.displayScale) private var displayScale
        @Environment(\.dynamicTypeSize) private var dynamicTypeSize
        /// 正在改名的工作区和输入框里的名字。
        @State private var renaming: SessionSection?
        @State private var renameText = ""

        var body: some View {
            ScrollViewReader { proxy in
                list(proxy)
            }
        }

        private func list(_ proxy: ScrollViewProxy) -> some View {
            List {
                if !model.linkState.isConnected {
                    Section {
                        ConnectionStatusRow(state: model.linkState, onRetry: model.reconnect)
                            .cardBackground()
                            .plainListRow()
                    }
                }
                let waiting = model.sections.flatMap { section in
                    section.sessions.filter { SessionGroup.of($0) == .waiting }.map { (section: section, session: $0) }
                }
                if !waiting.isEmpty {
                    Section {
                        WaitingSummary(items: waiting) { id in
                            withAnimation { proxy.scrollTo(id, anchor: .top) }
                        }
                    }
                }
                ForEach(model.sections) { section in
                    Section {
                        WorkspaceHeader(
                            section: section, canSpawn: model.canSpawn(in: section), isSpawning: model.isSpawning,
                            onSpawn: { Task { await model.spawn(in: section) } },
                            onRename: {
                                renameText = Presentation.sectionTitle(section)
                                renaming = section
                            })
                        if section.sessions.isEmpty {
                            Text("没有终端")
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                                .cardBackground()
                                .plainListRow()
                        }
                        ForEach(section.sessions, id: \.id) { session in
                            row(session, in: section)
                        }
                    }
                }
            }
            .cardList()
            .animation(.default, value: model.sections.map(\.sessions.count))
            .overlay {
                // 有空工作区时每一节自己写着「没有终端」，整页的空状态只在一节都没有时出。
                if model.loaded, model.sections.isEmpty, model.linkState.isConnected {
                    ContentUnavailableView {
                        Label("这台电脑上没有终端", systemImage: "terminal")
                    } description: {
                        Text("新开一个终端，在手机上就能用。")
                    } actions: {
                        Button("新开会话") { Task { await model.spawn() } }
                            .prominentButtonStyle()
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
                    ) { dir, name in
                        Task { await model.createWorkspace(at: dir, name: name) }
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
            // 按钮里用 `presenting` 带进来的节：对话框关掉时绑定先被清掉，不能再读 `renaming`。
            .alert(
                "给工作区改名", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } }),
                presenting: renaming
            ) { section in
                TextField("名字", text: $renameText)
                Button("取消", role: .cancel) {}
                Button("保存") {
                    let name = renameText
                    Task { await model.rename(section, to: name) }
                }
                .disabled(renameText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
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
        private func card(_ session: SessionInfo, in section: SessionSection) -> some View {
            let summary = SessionRow(
                session: session, workspaceDir: section.dir, preview: model.previews[session.id] ?? [],
                split: model.panes(sharingTabWith: session.id).first { $0.id == session.id })
            if SessionGroup.of(session) == .waiting {
                VStack(alignment: .leading, spacing: 10) {
                    Button { onOpen(session.id) } label: { summary.contentShape(Rectangle()) }
                        .buttonStyle(.plain)
                    QuickReplyBar(model: model.quickReply(for: session.id))
                        .accessibilityElement(children: .contain)
                        .accessibilityLabel("回复「\(Presentation.sessionTitle(session))」")
                }
                .cardBackground()
                // 等回答的卡片描一圈橙边，混在别的卡片里也一眼认得出。
                .overlay(RoundedRectangle.card.strokeBorder(AgentBadge.tint(for: .waiting).opacity(0.6), lineWidth: 1.5))
            } else {
                Button { onOpen(session.id) } label: { summary }
                    .buttonStyle(CardButtonStyle())
            }
        }

        /// 标题下面一行连接状态：连着时带地址。
        private var linkSubtitle: String {
            switch model.linkState {
            case .connected(_, let address?): String(localized: "已连接 · \(Presentation.displayAddress(address))")
            case .waiting: String(localized: "已断开，正在重连")
            default: Presentation.linkStatus(model.linkState)
            }
        }

        private func row(_ session: SessionInfo, in section: SessionSection) -> some View {
            card(session, in: section)
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
                    ForEach(sources, id: \.self) { source in
                        Menu {
                            ForEach(source.tasks, id: \.name) { task in
                                Button {
                                    Task {
                                        if await model.runProjectTask(task, at: source.project, in: session.id) { onRun() }
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

    /// 列表顶上的一张卡片：等你回答的会话一个一行，写着它在哪个工作区。点一行滚到它的卡片，快速回复在那里。
    private struct WaitingSummary: View {
        let items: [(section: SessionSection, session: SessionInfo)]
        let onSelect: (SessionId) -> Void
        @ScaledMetric(relativeTo: .subheadline) private var iconSize: CGFloat = 18

        var body: some View {
            VStack(alignment: .leading, spacing: 4) {
                Label("\(items.count) 个等你回答", systemImage: Presentation.symbol(for: .waiting))
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(AgentBadge.tint(for: .waiting))
                    .accessibilityAddTraits(.isHeader)
                ForEach(items, id: \.session.id) { item in
                    Button { onSelect(item.session.id) } label: {
                        HStack(spacing: 8) {
                            SessionIcon(agent: item.session.meta.agent?.kind, size: iconSize)
                            Text(Presentation.sessionTitle(item.session))
                                .lineLimit(1)
                            Spacer(minLength: 8)
                            Text(Presentation.sectionTitle(item.section))
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                            Image(systemName: "arrow.down")
                                .font(.footnote.weight(.semibold))
                                .foregroundStyle(.tertiary)
                                .accessibilityHidden(true)
                        }
                        .font(.subheadline)
                        .frame(minHeight: 36)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .accessibilityHint("滚到这个终端的卡片")
                }
            }
            .cardBackground()
            .overlay(RoundedRectangle.card.strokeBorder(AgentBadge.tint(for: .waiting).opacity(0.6), lineWidth: 1.5))
            .plainListRow()
        }
    }

    /// 一节的标题：工作区的名字和目录，右边一个在这个工作区里新开终端的按钮，长按菜单能新开终端、改名；
    /// 后台那一节只有标题。
    private struct WorkspaceHeader: View {
        let section: SessionSection
        let canSpawn: Bool
        let isSpawning: Bool
        let onSpawn: () -> Void
        let onRename: () -> Void

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
                if canSpawn {
                    Button("在这里新开终端", systemImage: "plus", action: onSpawn)
                        .labelStyle(.iconOnly)
                        .font(.body.weight(.semibold))
                        .buttonStyle(.borderless)
                        .disabled(isSpawning)
                        .accessibilityLabel("在「\(Presentation.sectionTitle(section))」里新开终端")
                }
            }
            .padding(.horizontal, 16)
            .padding(.top, 12)
            .contentShape(Rectangle())
            .contextMenu {
                if case .workspace = section.id {
                    if canSpawn {
                        Button("在这里新开终端", systemImage: "plus", action: onSpawn).disabled(isSpawning)
                    }
                    Button("改名", systemImage: "pencil", action: onRename)
                }
            }
            .plainListRow()
        }
    }

    private struct SessionRow: View {
        let session: SessionInfo
        /// 所在工作区的目录：会话就在这里时卡片上不再写一遍。
        let workspaceDir: String?
        let preview: [String]
        /// 电脑上它所在的标签分了屏时是它那个分屏，卡片上标出它在标签里的位置。
        var split: SplitPane?
        /// 标题前图标的边长，跟着标题的字号缩放。
        @ScaledMetric(relativeTo: .headline) private var iconSize: CGFloat = 22
        /// 状态图标定宽，转圈和月亮宽窄不一，定了宽各张卡片的标题才对齐。
        @ScaledMetric(relativeTo: .subheadline) private var stateIconWidth: CGFloat = 20

        var body: some View {
            VStack(alignment: .leading, spacing: 6) {
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    SessionIcon(agent: session.meta.agent?.kind, size: iconSize)
                        // 图标的中线对着第一行字的中间（标题的大写字母高约为图标边长的 0.54）。
                        .alignmentGuide(.firstTextBaseline) { [iconSize] d in d[VerticalAlignment.center] + iconSize * 0.27 }
                        .opacity(session.exited ? 0.5 : 1)
                    // 没有 agent 时不留这块空，shell 的标题紧挨着前面的图标。
                    if Presentation.agentStatus(session.meta.agent) != nil {
                        AgentStateIcon(agent: session.meta.agent)
                            .font(.subheadline.weight(.semibold))
                            .frame(width: stateIconWidth)
                    }
                    Text(Presentation.sessionTitle(session))
                        .font(.headline)
                        .foregroundStyle(session.exited ? .secondary : .primary)
                        .lineLimit(2)
                    if session.exited {
                        Text("已退出")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .padding(.horizontal, 6)
                            .background(.quaternary, in: Capsule())
                    }
                    if let split {
                        HStack(spacing: 4) {
                            PaneGlyph(rect: split.rect)
                            Text("分屏")
                        }
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .padding(.horizontal, 6)
                        .background(.quaternary, in: Capsule())
                        .fixedSize()
                    }
                    Spacer(minLength: 8)
                    DisclosureChevron()
                }
                if let directory = Presentation.sessionDirectory(session.meta.cwd, in: workspaceDir) {
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
