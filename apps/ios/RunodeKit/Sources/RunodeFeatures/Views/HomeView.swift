#if os(iOS)
    import RunodeConnection
    import RunodeProtocol
    import RunodeTerminal
    import SwiftUI
    import UIKit

    /// 首页：顶上是连着的电脑上加起来的统计，接着是等你回答的会话（带快速回复）、配对过的电脑、
    /// 上次打开的终端和快捷操作。配对过的电脑在 App 处于前台时都连着（见 `AppModel`）。
    struct HomeView: View {
        @Bindable var app: AppModel
        /// 就是 `app.machineList`，单独拿着才能给改名、删除的对话框做绑定。
        @Bindable var machines: MachineListModel
        @Environment(\.displayScale) private var displayScale
        @Environment(\.dynamicTypeSize) private var dynamicTypeSize
        @Environment(\.themeColors) private var colors
        @State private var choosingSpawnMachine = false
        /// 首页的大小，和终端页差不多；新开会话按它和设置里的字号算网格尺寸。
        @State private var pageSize: CGSize?

        var body: some View {
            content
                .leadingNavigationTitle("Runode", showsMark: true)
                .toolbar {
                    ToolbarItem(placement: .primaryAction) {
                        Button("设置", systemImage: "gearshape") { app.showingSettings = true }
                    }
                }
                .task(id: machines.machines.map(\.id)) { await app.keepHomeRefreshing() }
                .onGeometryChange(for: CGSize.self) { $0.size } action: { pageSize = $0 }
                // 按钮里用 `presenting` 带进来的编号：对话框关掉时绑定先被清掉，不能再从模型里读。
                .alert("改名", isPresented: $machines.isRenaming, presenting: machines.renameTarget) { id in
                    TextField("名字", text: $machines.renameText)
                    Button("取消", role: .cancel) {}
                    Button("保存") {
                        let name = machines.renameText
                        Task { await machines.rename(id, to: name) }
                    }
                }
                .alert("出错了", isPresented: showsError) {
                    Button("好") {}
                } message: {
                    Text(errorMessage ?? "")
                }
        }

        /// 有配对的电脑时是一列卡片；没有时是一屏不能滚动、不能下拉刷新的空状态。
        @ViewBuilder
        private var content: some View {
            if machines.machines.isEmpty {
                Group {
                    if machines.loaded {
                        // 一屏放得下时不滚动；字号调得很大、放不下时才能滚。
                        ViewThatFits(in: .vertical) {
                            Onboarding { app.startPairing() }
                            ScrollView { Onboarding { app.startPairing() } }
                        }
                    } else {
                        Color.clear
                    }
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(colors.page)
            } else {
                List {
                    summarySection
                    waitingSection
                    machineSection
                    resumeSection
                    actionSection
                }
                .cardList()
                .animation(.default, value: app.waitingSessions.map(\.id))
                .refreshable { app.refreshHome() }
            }
        }

        // MARK: 各块

        private var summarySection: some View {
            let summary = app.summary
            return Section {
                HStack(spacing: 10) {
                    StatTile(
                        value: summary.waiting, title: String(localized: "等你回答"), systemImage: Presentation.symbol(for: .waiting),
                        tint: summary.waiting > 0 ? .orange : .secondary)
                    StatTile(
                        value: summary.working, title: String(localized: "干活中"), systemImage: Presentation.symbol(for: .working),
                        tint: summary.working > 0 ? .primary : .secondary)
                    StatTile(
                        value: summary.sessions, title: String(localized: "会话"), systemImage: "terminal.fill",
                        tint: summary.sessions > 0 ? .primary : .secondary)
                }
                .padding(.top, 8)
                .plainListRow()
            }
        }

        @ViewBuilder
        private var waitingSection: some View {
            let waiting = app.waitingSessions
            if !waiting.isEmpty {
                Section {
                    ListSectionHeader(title: String(localized: "等你回答"))
                    ForEach(waiting) { item in
                        WaitingCard(item: item, showsMachine: machines.machines.count > 1) {
                            app.openTerminal(machine: item.list.machine.id, session: item.session.id)
                        }
                        .cardBackground()
                        .plainListRow()
                    }
                }
            }
        }

        private var machineSection: some View {
            Section {
                ListSectionHeader(title: String(localized: "电脑"))
                ForEach(machines.machines) { machine in
                    Button {
                        app.path = [.machine(machine.id)]
                    } label: {
                        MachineCard(machine: machine, list: app.sessionList(for: machine.id))
                    }
                    .buttonStyle(CardButtonStyle())
                    .plainListRow()
                    .swipeActions(edge: .trailing) {
                        // 不用 `.destructive`，理由同会话列表左滑结束：系统会先把这一行移走，确认框跟着消失。
                        Button("删除", systemImage: "trash") {
                            machines.deleteTarget = machine.id
                        }
                        .tint(.red)
                        Button("改名", systemImage: "pencil") {
                            machines.beginRename(machine.id)
                        }
                        .tint(.orange)
                    }
                    .contextMenu {
                        Button("打开", systemImage: "terminal") { app.path = [.machine(machine.id)] }
                        Button("改名", systemImage: "pencil") { machines.beginRename(machine.id) }
                        Button("删除", systemImage: "trash", role: .destructive) { machines.deleteTarget = machine.id }
                    }
                    // 确认框挂在卡片上：iOS 26 起它是指向所挂视图的气泡，挂在整页上会指到屏幕中间。
                    .confirmationDialog(
                        "删除这台电脑？", isPresented: confirmsDelete(machine.id), titleVisibility: .visible
                    ) {
                        Button("删除", role: .destructive) { Task { await machines.delete(machine.id) } }
                    } message: {
                        Text("会删掉这部手机上为它保存的设备密钥，以后要重新扫码配对。电脑上的配对记录请在那台电脑上撤销。")
                    }
                }
            }
        }

        /// 这台电脑是不是等着确认删除。
        private func confirmsDelete(_ id: UUID) -> Binding<Bool> {
            Binding(
                get: { machines.deleteTarget == id },
                set: { if !$0, machines.deleteTarget == id { machines.deleteTarget = nil } })
        }

        @ViewBuilder
        private var resumeSection: some View {
            if let resume = app.resume {
                Section {
                    ListSectionHeader(title: String(localized: "上次打开"))
                    Button(action: app.resumeRecent) {
                        ResumeCard(item: resume)
                    }
                    .buttonStyle(CardButtonStyle())
                    .plainListRow()
                }
            }
        }

        private var actionSection: some View {
            let spawnable = app.spawnableLists
            return Section {
                ListSectionHeader(title: String(localized: "快捷操作"))
                HStack(spacing: 10) {
                    ActionTile(title: String(localized: "配对电脑"), systemImage: "qrcode.viewfinder", busy: false) {
                        app.startPairing()
                    }
                    ActionTile(title: String(localized: "新开会话"), systemImage: "plus", busy: spawnable.contains(where: \.isSpawning)) {
                        if spawnable.count == 1, let list = spawnable.first {
                            spawn(on: list)
                        } else {
                            choosingSpawnMachine = true
                        }
                    }
                    .disabled(spawnable.isEmpty)
                    .confirmationDialog(
                        "在哪台电脑上新开终端？", isPresented: $choosingSpawnMachine, titleVisibility: .visible
                    ) {
                        ForEach(app.spawnableLists, id: \.machine.id) { list in
                            Button(list.machine.name) { spawn(on: list) }
                        }
                    }
                }
                .padding(.bottom, 16)
                .plainListRow()
            }
        }

        private func spawn(on list: SessionListModel) {
            if let pageSize {
                list.spawnSize = TerminalView.gridSize(
                    fitting: pageSize, scale: displayScale, contentSize: UIContentSizeCategory(dynamicTypeSize),
                    fontSize: app.settings.preferences.fontSize.map { CGFloat($0) })
            }
            Task { await list.spawn() }
        }

        // MARK: 出错

        /// 首页上新开会话出的错。首页被别的页盖住时不弹，留给那一页自己的提示。
        private var spawnErrorList: SessionListModel? {
            guard app.path.isEmpty else { return nil }
            return app.machineLists.first { $0.errorMessage != nil }
        }

        private var errorMessage: String? {
            machines.errorMessage ?? spawnErrorList?.errorMessage
        }

        private var showsError: Binding<Bool> {
            Binding(
                get: { errorMessage != nil },
                set: { shown in
                    guard !shown else { return }
                    if machines.errorMessage != nil {
                        machines.errorMessage = nil
                    } else {
                        spawnErrorList?.errorMessage = nil
                    }
                })
        }
    }

    /// 还没配对电脑时的首页：中间一句话说清这是干什么的和配对按钮，下面三步说明怎么配。
    private struct Onboarding: View {
        let onPair: () -> Void

        var body: some View {
            VStack(spacing: 0) {
                Spacer(minLength: 32)
                VStack(spacing: 12) {
                    Text("连接你的电脑")
                        .font(.title2.weight(.bold))
                    Text("和电脑上的 Runode 配对，在手机上看 agent 在做什么、回答它们的提问，打开任何一个终端接着干活。")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                    Button(action: onPair) {
                        Label("配对电脑", systemImage: "qrcode.viewfinder")
                            .font(.headline)
                            .padding(.horizontal, 24)
                            .frame(minHeight: 48)
                            .foregroundStyle(Color(.systemBackground))
                            .background(Color.primary, in: .inner)
                            .contentShape(.inner)
                    }
                    .buttonStyle(.plain)
                    .padding(.top, 12)
                }
                .multilineTextAlignment(.center)
                .padding(.horizontal, 32)
                Spacer(minLength: 32)
                steps
            }
            .padding(.bottom, 16)
        }

        private var steps: some View {
            VStack(alignment: .leading, spacing: 0) {
                Text("怎么配对")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(.secondary)
                    .padding(.bottom, 10)
                    .accessibilityAddTraits(.isHeader)
                OnboardingStep(
                    number: 1, title: String(localized: "运行配对命令"),
                    detail: "在电脑的终端里运行 `runode\u{00A0}remote\u{00A0}pair`，屏幕上会出现一个二维码。")
                Divider().padding(.leading, 40)
                OnboardingStep(
                    number: 2, title: String(localized: "扫码"),
                    detail: "点上面的按钮打开扫码，对准电脑屏幕上的二维码。配好后电脑就出现在这里。")
            }
            .padding(.horizontal, 24)
        }
    }

    /// 配对说明里的一步：左边编号，右边标题和一行说明（说明里能用 Markdown 的行内代码）。
    private struct OnboardingStep: View {
        let number: Int
        let title: String
        let detail: LocalizedStringKey
        @Environment(\.themeColors) private var colors

        var body: some View {
            HStack(alignment: .top, spacing: 12) {
                Text(number, format: .number)
                    .font(.footnote.weight(.semibold).monospacedDigit())
                    .foregroundStyle(.secondary)
                    .frame(width: 28, height: 28)
                    .background(colors.fill, in: .inner)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                        .font(.subheadline.weight(.semibold))
                    Text(detail)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(.vertical, 12)
            .accessibilityElement(children: .combine)
        }
    }

    /// 卡片左边的图标：浅底圆角方块。
    private struct IconTile: View {
        let systemImage: String
        var tint: Color = .secondary
        @Environment(\.themeColors) private var colors

        var body: some View {
            Image(systemName: systemImage)
                .font(.title3)
                .foregroundStyle(tint)
                .frame(width: 40, height: 40)
                .background(colors.fill, in: .inner)
                .accessibilityHidden(true)
        }
    }

    /// 统计那一排里的一格：大数字，右上角一个图标，下面一行说明。数是零时整格用灰色，一眼看出哪项有事。
    private struct StatTile: View {
        let value: Int
        let title: String
        let systemImage: String
        let tint: Color
        @Environment(\.themeColors) private var colors

        var body: some View {
            VStack(alignment: .leading, spacing: 2) {
                HStack(alignment: .firstTextBaseline) {
                    Text(value, format: .number)
                        .font(.title2.weight(.bold).monospacedDigit())
                        .contentTransition(.numericText(value: Double(value)))
                    Spacer(minLength: 4)
                    Image(systemName: systemImage)
                        .font(.footnote)
                        .accessibilityHidden(true)
                }
                .foregroundStyle(tint)
                Text(title)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(12)
            .background(colors.card, in: .card)
            .animation(.default, value: value)
            .accessibilityElement(children: .ignore)
            .accessibilityLabel("\(title) \(value)")
        }
    }

    /// 快捷操作那一排里的一格。
    private struct ActionTile: View {
        let title: String
        let systemImage: String
        let busy: Bool
        let action: () -> Void
        @Environment(\.isEnabled) private var isEnabled
        @Environment(\.themeColors) private var colors

        var body: some View {
            Button(action: action) {
                HStack(spacing: 10) {
                    Group {
                        if busy {
                            ProgressView()
                        } else {
                            Image(systemName: systemImage)
                                .font(.body.weight(.semibold))
                        }
                    }
                    .frame(width: 32, height: 32)
                    .background(colors.fill, in: .inner)
                    Text(title)
                        .font(.subheadline.weight(.semibold))
                        .lineLimit(1)
                        .minimumScaleFactor(0.8)
                    Spacer(minLength: 0)
                }
                .padding(12)
                .frame(maxWidth: .infinity, minHeight: 56)
                .background(colors.card, in: .card)
                .contentShape(.card)
            }
            .buttonStyle(.plain)
            .foregroundStyle(isEnabled ? Color.primary : Color.secondary)
            .disabled(busy)
        }
    }

    /// 连接状态前面的小圆点：连着绿，等着重连橙，失败红，其余灰。
    private struct StatusDot: View {
        let state: LinkState

        var body: some View {
            Circle()
                .fill(color)
                .frame(width: 8, height: 8)
                .accessibilityHidden(true)
        }

        private var color: Color {
            switch state {
            case .connected: .green
            case .waiting: .orange
            case .failed: .red
            case .idle, .connecting: .secondary
            }
        }
    }

    /// 一台电脑：名字，下面是连接状态和会话数。
    private struct MachineCard: View {
        let machine: MachineRecord
        let list: SessionListModel?

        var body: some View {
            let state = list?.linkState ?? .idle
            HStack(spacing: 12) {
                IconTile(systemImage: "laptopcomputer", tint: state.isConnected ? .primary : .secondary)
                VStack(alignment: .leading, spacing: 3) {
                    Text(machine.name)
                        .font(.headline)
                        .foregroundStyle(state.isConnected ? .primary : .secondary)
                        .lineLimit(1)
                    HStack(alignment: .firstTextBaseline, spacing: 6) {
                        StatusDot(state: state)
                        TimelineView(.periodic(from: .now, by: 1)) { context in
                            Text(
                                Presentation.machineSummary(
                                    state, sessions: list?.sessions ?? [], loaded: list?.loaded ?? false,
                                    now: context.date))
                        }
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                    }
                }
                Spacer(minLength: 8)
                DisclosureChevron()
            }
            .accessibilityElement(children: .combine)
            .accessibilityHint("打开这台电脑上的会话")
        }
    }

    /// 上次打开的终端。
    private struct ResumeCard: View {
        let item: ResumeItem

        var body: some View {
            HStack(spacing: 12) {
                IconTile(systemImage: item.agent == nil ? "apple.terminal" : "sparkles", tint: .primary)
                VStack(alignment: .leading, spacing: 3) {
                    Text(item.title)
                        .font(.headline)
                        .lineLimit(1)
                    Text(subtitle)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
                Spacer(minLength: 8)
                DisclosureChevron()
            }
            .accessibilityElement(children: .combine)
            .accessibilityHint("打开这个终端")
        }

        private var subtitle: String {
            var parts = [item.machine.name]
            if let status = Presentation.agentStatus(item.agent) { parts.append(status.text) }
            if let directory = Presentation.directory(item.directory) { parts.append(directory) }
            return parts.joined(separator: " · ")
        }
    }

    /// 等你回答的一个会话：点上半部分打开终端，下面直接快速回复。
    private struct WaitingCard: View {
        let item: WaitingSession
        let showsMachine: Bool
        let onOpen: () -> Void

        var body: some View {
            VStack(alignment: .leading, spacing: 10) {
                Button(action: onOpen) {
                    HStack(alignment: .top, spacing: 8) {
                        VStack(alignment: .leading, spacing: 6) {
                            Text(Presentation.sessionTitle(item.session))
                                .font(.headline)
                                .lineLimit(2)
                            AgentBadge(agent: item.session.meta.agent, showsState: false)
                            if let place {
                                Text(place)
                                    .font(.footnote)
                                    .foregroundStyle(.secondary)
                                    .lineLimit(1)
                                    .truncationMode(.head)
                            }
                            let preview = item.list.previews[item.session.id] ?? []
                            if !preview.isEmpty {
                                ScreenPreview(lines: preview)
                            }
                        }
                        Spacer(minLength: 0)
                        DisclosureChevron()
                            .padding(.top, 4)
                    }
                    .contentShape(Rectangle())
                    .accessibilityElement(children: .combine)
                    .accessibilityHint("打开这个终端")
                }
                .buttonStyle(.plain)
                QuickReplyBar(model: item.list.quickReply(for: item.session.id))
                    .accessibilityElement(children: .contain)
                    .accessibilityLabel("回复「\(Presentation.sessionTitle(item.session))」")
            }
        }

        /// 在哪台电脑的哪个目录：只配对了一台电脑时不写电脑的名字。
        private var place: String? {
            let parts = [showsMachine ? item.list.machine.name : nil, Presentation.directory(item.session.meta.cwd)]
                .compactMap { $0 }
            return parts.isEmpty ? nil : parts.joined(separator: " · ")
        }
    }
#endif
