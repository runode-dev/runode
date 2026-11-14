#if os(iOS)
    import RunodeConnection
    import RunodeProtocol
    import RunodeTerminal
    import SwiftUI
    import UIKit

    /// 一台电脑上的会话，按工作区分节（分节见 `SessionListModel.sections`），节头能新开终端、长按改名。
    /// 顶上能筛出只看等你回答的、在干活的。一个会话一张卡片，样子按 `SessionStatus` 分：等你回答的带屏幕
    /// 摘录和一排快速回复的按键，在干活的带屏幕最后一行，其余只有标题和状态；点开终端，左滑结束，长按有
    /// 更多操作。右上角新开终端、新建工作区，打开远程桌面。
    struct SessionListView: View {
        @Bindable var model: SessionListModel
        let onOpen: (SessionId) -> Void
        /// 打开会话所在仓库的 Git 页。
        var onOpenGit: (SessionId) -> Void = { _ in }
        /// 打开这台电脑的远程桌面；为空时没有这个入口。
        var onOpenScreen: (() -> Void)? = nil
        /// 设置里的手动字号，为空时跟随动态字体。
        var fontSize: Double? = nil
        @Environment(\.displayScale) private var displayScale
        @Environment(\.dynamicTypeSize) private var dynamicTypeSize
        @Environment(\.appTheme) private var theme
        /// 页面的大小，后台新开会话时按它和字号算网格（`spawnSize`）。
        @State private var pageSize: CGSize?
        /// 正在改名的工作区和输入框里的名字。
        @State private var renaming: SessionSection?
        @State private var renameText = ""
        @State private var filter = SessionFilter.all

        /// 和打开终端后适配的网格一样大，宿主起会话时就不用再改尺寸。
        private func updateSpawnSize() {
            guard let pageSize else { return }
            model.spawnSize = TerminalView.gridSize(
                fitting: pageSize, scale: displayScale, contentSize: UIContentSizeCategory(dynamicTypeSize),
                fontSize: fontSize.map { CGFloat($0) })
        }

        /// 「新建」菜单里新开终端（`agent` 为空）或起一家 agent 的一项：能新开终端的工作区不止一个时是挑工作区的
        /// 子菜单，否则直接开在电脑上当前的工作区里。
        @ViewBuilder
        private func spawnItem(agent: InstalledAgent?, @ViewBuilder label: () -> some View) -> some View {
            let choices = model.spawnChoices
            if choices.isEmpty {
                Button {
                    Task { await model.spawn(agent: agent) }
                } label: {
                    label()
                }
            } else {
                Menu {
                    ForEach(choices) { section in
                        Button {
                            Task { await model.spawn(in: section, agent: agent) }
                        } label: {
                            Text(Presentation.sectionTitle(section))
                            if let detail = Presentation.sectionDetail(section) { Text(detail) }
                        }
                    }
                } label: {
                    label()
                }
            }
        }

        var body: some View {
            let sections = model.sections(matching: filter)
            List {
                // 两行放在同一节里：普通样式的列表只在同一节的行之间留 `listRowSpacing` 的空，节与节之间不留。
                Section {
                    if !model.linkState.isConnected {
                        ConnectionStatusRow(state: model.linkState, onRetry: model.reconnect)
                            .cardBackground()
                            .plainListRow()
                    }
                    // 一个会话都没有时不出筛选条；筛着的时候会话没了也留着，才切得回全部。
                    if !model.sessions.isEmpty || filter != .all {
                        SessionFilterBar(selection: $filter, count: model.count)
                            .plainListRow()
                    }
                }
                if sections.isEmpty, filter != .all {
                    Section {
                        FilterEmptyRow(filter: filter) { filter = .all }
                    }
                }
                ForEach(sections) { section in
                    Section {
                        WorkspaceHeader(
                            section: section, canSpawn: model.canSpawn(in: section), isSpawning: model.isSpawning,
                            agents: model.agents,
                            onSpawn: { agent in Task { await model.spawn(in: section, agent: agent) } },
                            onRename: {
                                renameText = Presentation.sectionTitle(section)
                                renaming = section
                            })
                        ForEach(section.sessions, id: \.id) { session in
                            row(session, in: section)
                        }
                    }
                }
            }
            .cardList()
            .animation(.default, value: sections.map(\.sessions.count))
            .overlay {
                // 有空工作区时每一节自己写着「没有终端」，整页的空状态只在一节都没有时出。
                if model.loaded, model.sections.isEmpty, model.linkState.isConnected {
                    ContentUnavailableView {
                        Label("这台电脑上没有终端", systemImage: "terminal")
                    } description: {
                        Text("新开一个终端，在手机上就能用。")
                    } actions: {
                        Group {
                            if model.agents.isEmpty {
                                Button("新开终端") { Task { await model.spawn() } }
                            } else {
                                Menu("新开终端") {
                                    SpawnKindItems(agents: model.agents) { agent in
                                        Task { await model.spawn(agent: agent) }
                                    }
                                }
                            }
                        }
                        .prominentButtonStyle()
                        .controlSize(.large)
                    }
                } else if !model.loaded, model.linkState.isConnected {
                    ProgressView("正在读取会话…")
                }
            }
            // 「已连接」在前、地址在后，截开头会把连接状态截掉。
            .leadingNavigationTitle(model.machine.name, subtitle: linkSubtitle, subtitleTruncation: .middle)
            .toolbar {
                if let onOpenScreen {
                    ToolbarItem(placement: .primaryAction) {
                        Button("远程桌面", systemImage: "display", action: onOpenScreen)
                            .disabled(!model.linkState.isConnected)
                    }
                }
                NewMenuItem {
                    Menu {
                        Section {
                            spawnItem(agent: nil) {
                                Label("新开终端", systemImage: "terminal")
                            }
                            ForEach(model.agents) { agent in
                                spawnItem(agent: agent) {
                                    AgentMenuLabel(title: String(localized: "新开 \(agent.label)"), kind: agent.kind)
                                }
                            }
                        }
                        Button("新建工作区", systemImage: "folder.badge.plus") {
                            model.beginNewWorkspace()
                        }
                    } label: {
                        NewMenuLabel()
                    }
                    .modifier(ProminentToolbarButton())
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
                    .appTheme(theme)
                }
            }
            .refreshable {
                model.refresh()
                model.findAgents()
                model.refreshPreviews()
                await model.refreshTasks()
            }
            .task { await model.keepRefreshing() }
            .onGeometryChange(for: CGSize.self) { $0.size } action: { size in
                pageSize = size
                updateSpawnSize()
            }
            .onChange(of: fontSize) { updateSpawnSize() }
            .onChange(of: dynamicTypeSize) { updateSpawnSize() }
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

        /// 一个会话一张卡片，样子按 `SessionStatus` 分。等你回答的卡片只有头部能点开终端，下面的按键是快速
        /// 回复，两者不能套在一个按钮里；别的卡片整张能点开。
        @ViewBuilder
        private func card(_ session: SessionInfo, in section: SessionSection) -> some View {
            let head = SessionRow(session: session, workspaceDir: section.dir, tab: model.tabNumber(of: session.id))
            let preview = model.previews[session.id] ?? []
            switch SessionStatus.of(session) {
            case .waiting:
                // 认出了提问时选项单独成键，摘录只放问题。
                let prompt = model.prompts[session.id]
                let excerpt = prompt?.question ?? preview
                VStack(alignment: .leading, spacing: 8) {
                    Button { onOpen(session.id) } label: { head.contentShape(Rectangle()) }
                        .buttonStyle(.plain)
                    if !excerpt.isEmpty {
                        ScreenPreview(lines: excerpt, tone: .primary)
                    }
                    QuickReplyBar(
                        model: model.quickReply(for: session.id), prompt: prompt, optionLayout: .compact,
                        showsDraft: false)
                        .accessibilityElement(children: .contain)
                        .accessibilityLabel("回复「\(Presentation.sessionTitle(session))」")
                }
                .cardBackground(padding: 12)
            case .working:
                Button { onOpen(session.id) } label: {
                    VStack(alignment: .leading, spacing: 8) {
                        head
                        if !preview.isEmpty {
                            ScreenPreview(lines: Array(preview.suffix(1)))
                        }
                    }
                }
                .buttonStyle(CardButtonStyle())
            case let status:
                Button { onOpen(session.id) } label: {
                    head
                        .padding(.vertical, 8)
                        .padding(.horizontal, 12)
                }
                .buttonStyle(CardButtonStyle(padding: 0))
                .opacity(status == .exited ? 0.6 : 1)
            }
        }

        private var linkSubtitle: String {
            Presentation.linkRoute(model.linkState)
        }

        private func row(_ session: SessionInfo, in section: SessionSection) -> some View {
            card(session, in: section)
                .plainListRow()
                .trailingSwipeActions {
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
                    TasksSection(model: model, session: session) { onOpen(session.id) }
                    Section(
                        "\(Presentation.gridSize(session.size))，\(Presentation.sizeOwner(session.sizeOwner))"
                    ) {
                        Button("结束会话", systemImage: "xmark.circle", role: .destructive) {
                            model.killTarget = session.id
                        }
                    }
                }
                .task(id: session.meta.cwd) {
                    if let cwd = session.meta.cwd { await model.loadTasks(in: cwd) }
                }
                // 挂在卡片上：iOS 26 起确认框是指向所挂视图的气泡，挂在整个列表上会指到屏幕中间。
                .confirmationDialog(
                    "结束这个终端？", isPresented: confirmsKill(session.id), titleVisibility: .visible
                ) {
                    Button("结束会话", role: .destructive) { model.kill(session.id) }
                } message: {
                    Text("「\(Presentation.sessionTitle(session))」在电脑上也会关掉，里面正在跑的程序跟着结束。")
                }
        }

        /// 这个会话是不是等着确认结束。
        private func confirmsKill(_ id: SessionId) -> Binding<Bool> {
            Binding(get: { model.killTarget == id }, set: { if !$0, model.killTarget == id { model.killTarget = nil } })
        }
    }

    /// 菜单里会话目录的命令：Makefile、package.json 各一个子菜单，点一条就跑（见
    /// `SessionListModel.runTask`），在这个会话里跑了时调 `onRun`，在新终端里跑时由会话列表打开
    /// 新终端。终端结束了时子菜单点不开。会话卡片的长按菜单和终端页的「⋯」菜单都用它。
    struct TasksSection: View {
        let model: SessionListModel
        let session: SessionInfo
        var onRun: () -> Void = {}

        var body: some View {
            let sources = model.tasks(for: session).filter { !$0.tasks.isEmpty }
            if !sources.isEmpty {
                let runnable = model.canRunTask(in: session)
                Section(Presentation.tasksHeader(session)) {
                    ForEach(sources, id: \.self) { source in
                        Menu {
                            ForEach(source.tasks, id: \.name) { task in
                                Button {
                                    Task {
                                        if await model.runTask(task, at: source.project, in: session.id) { onRun() }
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

    /// 列表顶上的分段筛选：全部、等你、干活中，各带会话数。等你的有会话时那一段的字一律是琥珀色，没选中
    /// 也看得出有人在等。字号到了无障碍档时三段竖着排。
    private struct SessionFilterBar: View {
        @Binding var selection: SessionFilter
        let count: (SessionFilter) -> Int
        @Environment(\.themeColors) private var colors
        @Environment(\.dynamicTypeSize) private var dynamicTypeSize

        private static let outerRadius: CGFloat = 14
        private static let segmentRadius: CGFloat = 12
        private static let segmentHeight: CGFloat = 38

        var body: some View {
            let shape = RoundedRectangle(cornerRadius: Self.outerRadius, style: .continuous)
            let layout =
                dynamicTypeSize.isAccessibilitySize
                ? AnyLayout(VStackLayout(spacing: 2)) : AnyLayout(HStackLayout(spacing: 2))
            layout {
                ForEach(SessionFilter.allCases, id: \.self) { filter in
                    segment(filter)
                }
            }
            .padding(2)
            .background(colors.well, in: shape)
            .overlay(shape.strokeBorder(colors.hairline, lineWidth: 1).allowsHitTesting(false))
            .accessibilityElement(children: .contain)
            .accessibilityLabel("筛选会话")
        }

        private func segment(_ filter: SessionFilter) -> some View {
            let selected = selection == filter
            let number = count(filter)
            let title = Self.title(filter)
            let shape = RoundedRectangle(cornerRadius: Self.segmentRadius, style: .continuous)
            let alert = filter == .waiting && number > 0
            return Button {
                selection = filter
            } label: {
                HStack(spacing: 6) {
                    Text(title)
                        .font(.subheadline.weight(selected ? .semibold : .regular))
                        .foregroundStyle(
                            alert
                                ? AnyShapeStyle(colors.waiting)
                                : selected ? AnyShapeStyle(.primary) : AnyShapeStyle(.secondary))
                    Text(verbatim: String(number))
                        .font(.footnote.monospacedDigit())
                        .foregroundStyle(alert ? AnyShapeStyle(colors.waiting) : AnyShapeStyle(.secondary))
                }
                .lineLimit(1)
                .frame(maxWidth: .infinity, minHeight: Self.segmentHeight)
                .background(selected ? colors.raised : .clear, in: shape)
                .contentShape(shape)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("\(title)，\(number) 个")
            .accessibilityAddTraits(selected ? .isSelected : [])
        }

        private static func title(_ filter: SessionFilter) -> String {
            switch filter {
            case .all: String(localized: "全部")
            case .waiting: String(localized: "等你")
            case .working: String(localized: "干活中")
            }
        }
    }

    /// 筛出来一个会话都没有时列表里的那一行，带一个回到全部的按钮。
    private struct FilterEmptyRow: View {
        let filter: SessionFilter
        let onShowAll: () -> Void

        var body: some View {
            VStack(spacing: 12) {
                Text(message)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                Button(action: onShowAll) {
                    Text("看全部")
                        .padding(.horizontal, 16)
                }
                .buttonStyle(SurfaceButtonStyle(.raised))
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 24)
            .plainListRow()
        }

        private var message: String {
            switch filter {
            case .waiting: String(localized: "没有在等你的会话")
            case .all, .working: String(localized: "没有在干活的会话")
            }
        }
    }

    /// 新建菜单的标签：强调色的实心圆，图标用页面的底色。系统的实心样式图标固定是白的，前景色是白的时会
    /// 看不见，所以 iOS 26 上自己画圆；圆画在标签里，工具栏只照标签画菜单，套在菜单外面的底色不画。
    private struct NewMenuLabel: View {
        @Environment(\.isEnabled) private var isEnabled
        @Environment(\.themeColors) private var colors

        /// 和导航栏上别的按钮的玻璃底一样大。
        private static let diameter: CGFloat = 44

        var body: some View {
            if #available(iOS 26, *) {
                Image(systemName: "plus")
                    .fontWeight(.semibold)
                    .foregroundStyle(colors.page)
                    .frame(width: Self.diameter, height: Self.diameter)
                    .background(.tint, in: Circle())
                    .contentShape(Circle())
                    .opacity(isEnabled ? 1 : 0.4)
                    .accessibilityLabel("新建")
            } else {
                Label("新建", systemImage: "plus")
            }
        }
    }

    /// 新建菜单在工具栏上的按钮样式；iOS 26 上圆由 `NewMenuLabel` 画，所在的工具栏项要去掉系统的玻璃底
    /// （`NewMenuItem`）。
    private struct ProminentToolbarButton: ViewModifier {
        @Environment(\.isEnabled) private var isEnabled
        @Environment(\.themeColors) private var colors

        @ViewBuilder
        func body(content: Content) -> some View {
            if #available(iOS 26, *) {
                content.buttonStyle(.plain)
            } else {
                content
                    .buttonStyle(.borderedProminent)
                    .buttonBorderShape(.circle)
                    .foregroundStyle(isEnabled ? AnyShapeStyle(colors.page) : AnyShapeStyle(.secondary))
            }
        }
    }

    /// 放新建菜单的工具栏项。
    private struct NewMenuItem<Content: View>: ToolbarContent {
        @ViewBuilder let content: () -> Content

        var body: some ToolbarContent {
            if #available(iOS 26, *) {
                item.sharedBackgroundVisibility(.hidden)
            } else {
                item
            }
        }

        private var item: some ToolbarContent {
            ToolbarItem(placement: .primaryAction) { content() }
        }
    }

    /// 一节的标题：工作区的名字和目录，右边一个在这个工作区里新开终端的按钮（电脑上装着 agent 时是挑开终端
    /// 还是哪家 agent 的菜单），长按菜单能新开终端、改名；后台那一节只有标题。
    private struct WorkspaceHeader: View {
        let section: SessionSection
        let canSpawn: Bool
        let isSpawning: Bool
        let agents: [InstalledAgent]
        /// 收到的为空时开普通终端，否则起这家 agent。
        let onSpawn: (InstalledAgent?) -> Void
        let onRename: () -> Void
        /// 图标定宽，文件夹和后台那一节的月亮宽窄不一，定了宽各节的标题才对齐。
        @ScaledMetric(relativeTo: .body) private var iconWidth: CGFloat = 20

        var body: some View {
            VStack(alignment: .leading, spacing: 0) {
                HStack(spacing: 8) {
                    Image(systemName: Presentation.sectionSymbol(section))
                        .font(.body)
                        .foregroundStyle(.secondary)
                        .frame(width: iconWidth)
                        .accessibilityHidden(true)
                    Text(Presentation.sectionTitle(section))
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(.primary)
                        .lineLimit(1)
                        .layoutPriority(1)
                        .accessibilityAddTraits(.isHeader)
                    if let directory = Presentation.sectionDirectory(section) {
                        Text(directory)
                            .font(.caption.monospaced())
                            .foregroundStyle(.tertiary)
                            .lineLimit(1)
                            .truncationMode(.head)
                    }
                    if let window = Presentation.sectionWindow(section) {
                        Text(window)
                            .font(.caption)
                            .foregroundStyle(.tertiary)
                            .lineLimit(1)
                            .fixedSize()
                    }
                    Spacer(minLength: 8)
                    if canSpawn {
                        Group {
                            if agents.isEmpty {
                                Button { onSpawn(nil) } label: { spawnLabel }
                            } else {
                                Menu {
                                    SpawnKindItems(agents: agents, action: onSpawn)
                                } label: {
                                    spawnLabel
                                }
                            }
                        }
                        .font(.body.weight(.semibold))
                        .foregroundStyle(.secondary)
                        .buttonStyle(.borderless)
                        .disabled(isSpawning)
                        .accessibilityLabel("在「\(Presentation.sectionTitle(section))」里新开终端")
                    }
                }
                .frame(minHeight: 32)
                // 后台那一节没有目录，说明它是什么的那句另起一行；空工作区不另起一张卡片，也在这里写一行，
                // 一屏才放得下有会话的那几节。两行都和标题的字左对齐。
                Group {
                    if case .background = section.id, let detail = Presentation.sectionDetail(section) {
                        Text(detail)
                    }
                    if section.sessions.isEmpty {
                        Text("没有终端")
                    }
                }
                .font(.caption)
                .foregroundStyle(.tertiary)
                .padding(.leading, iconWidth + 8)
            }
            .padding(.top, 2)
            .padding(.horizontal, 4)
            .contentShape(Rectangle())
            .contextMenu {
                if case .workspace = section.id {
                    if canSpawn {
                        if agents.isEmpty {
                            Button("在这里新开终端", systemImage: "plus") { onSpawn(nil) }.disabled(isSpawning)
                        } else {
                            Menu("在这里新开终端", systemImage: "plus") {
                                SpawnKindItems(agents: agents, action: onSpawn)
                            }
                            .disabled(isSpawning)
                        }
                    }
                    Button("改名", systemImage: "pencil", action: onRename)
                }
            }
            .plainListRow()
        }

        private var spawnLabel: some View {
            Label("在这里新开终端", systemImage: "plus")
                .labelStyle(.iconOnly)
                .frame(minWidth: 44, minHeight: 32, alignment: .trailing)
                .contentShape(Rectangle())
        }
    }

    /// 会话卡片的头部：状态标记，标题，下面一行说状态（`Presentation.sessionSubtitle`），末尾是标签页号；
    /// 会话不在工作区的目录里时再起一行写它的目录。状态由那行字说，读屏不念标记。
    private struct SessionRow: View {
        let session: SessionInfo
        /// 所在工作区的目录：会话就在这里时卡片上不再写一遍。
        let workspaceDir: String?
        /// 电脑上它在工作区里是第几个标签页，同一个工作区的几张卡片靠它分得开。
        var tab: Int?
        @ScaledMetric(relativeTo: .callout) private var glyphSize: CGFloat = 18

        var body: some View {
            HStack(spacing: 10) {
                StatusGlyph(StatusMark(SessionStatus.of(session)), size: glyphSize)
                VStack(alignment: .leading, spacing: 1) {
                    Text(Presentation.sessionTitle(session))
                        .font(.callout.weight(.semibold))
                        .foregroundStyle(.primary)
                        .lineLimit(2)
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        TimelineView(AgentClockSchedule(session)) { context in
                            Text(Presentation.sessionSubtitle(session, now: context.date))
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                        }
                        if let tab {
                            Text("标签页 \(tab)")
                                .font(.caption)
                                .foregroundStyle(.tertiary)
                                .lineLimit(1)
                                .fixedSize()
                        }
                    }
                    if let directory = Presentation.sessionDirectory(session.meta.cwd, in: workspaceDir) {
                        Text(directory)
                            .font(.caption.monospaced())
                            .foregroundStyle(.tertiary)
                            .lineLimit(1)
                            .truncationMode(.head)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                DisclosureChevron()
            }
            .frame(minHeight: 42)
            .padding(.horizontal, 2)
            .accessibilityElement(children: .combine)
            .accessibilityHint("打开这个终端")
        }
    }
#endif
