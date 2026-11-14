#if os(iOS)
    import RunodeConnection
    import RunodeProtocol
    import RunodeTerminal
    import SwiftUI
    import UIKit

    /// 首页：顶上一句话说有几个会话在等你，接着是等你回答的会话（一次摊开一张，带快速回复）、配对过的
    /// 电脑和上次打开的终端，底下钉着配对和新开终端两个按钮。配对过的电脑在 App 处于前台时都连着
    /// （`AppModel.keepHomeRefreshing`）。
    struct HomeView: View {
        @Bindable var app: AppModel
        /// 就是 `app.machineList`，单独拿着才能给改名、删除的对话框做绑定。
        @Bindable var machines: MachineListModel
        @Environment(\.displayScale) private var displayScale
        @Environment(\.dynamicTypeSize) private var dynamicTypeSize
        @Environment(\.themeColors) private var colors
        @ScaledMetric(relativeTo: .subheadline) private var pairIconSize: CGFloat = 17
        @ScaledMetric(relativeTo: .subheadline) private var spawnIconSize: CGFloat = 16
        /// 首页的大小，和终端页差不多；新开会话按它和设置里的字号算网格尺寸。
        @State private var pageSize: CGSize?
        /// 「等你回答」里用户翻到的那张卡（`WaitingSession.id`），由 `HomePager` 解读。
        @State private var focusedWaiting: String?

        var body: some View {
            content
                .leadingNavigationTitle("Runode", showsMark: true)
                .toolbar {
                    ToolbarItem(placement: .primaryAction) {
                        Button("设置", systemImage: "slider.horizontal.3") { app.showingSettings = true }
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
                    headlineSection
                    waitingSection
                    machineSection
                    resumeSection
                }
                .cardList()
                .animation(.default, value: app.waitingSessions.map(\.id))
                .refreshable { app.refreshHome() }
                .safeAreaInset(edge: .bottom) { actionBar }
            }
        }

        // MARK: 各块

        /// 底栏那一排：字号到了无障碍档时横着放不下，改成竖排。
        private var tileRow: AnyLayout {
            dynamicTypeSize.isAccessibilitySize ? AnyLayout(VStackLayout(spacing: 10)) : AnyLayout(HStackLayout(spacing: 10))
        }

        private var headlineSection: some View {
            Section {
                // 大标题是一页的开头，和下面的卡片、分组隔得比卡片之间开。
                HomeHeadline(summary: app.summary)
                    .padding(.bottom, 8)
                    .plainListRow()
            }
        }

        @ViewBuilder
        private var waitingSection: some View {
            let waiting = app.waitingSessions
            let ids = waiting.map(\.id)
            if let current = HomePager.current(focusedWaiting, in: ids), let index = ids.firstIndex(of: current) {
                let item = waiting[index]
                Section {
                    VStack(spacing: 0) {
                        WaitingCard(
                            item: item, showsMachine: machines.machines.count > 1, position: index + 1,
                            count: waiting.count,
                            onOpen: { app.openTerminal(machine: item.list.machine.id, session: item.session.id) },
                            onNext: { focusedWaiting = HomePager.next(after: current, in: ids) },
                            onPrevious: { focusedWaiting = HomePager.previous(before: current, in: ids) }
                        )
                        .cardBackground(padding: 14)
                        .animation(.default, value: item.id)
                        // 盖在下面那截卡边的上沿上。
                        .zIndex(1)
                        if waiting.count > 1 {
                            StackEdge()
                        }
                    }
                    .plainListRow()
                }
            }
        }

        private var machineSection: some View {
            let all = machines.machines
            return Section {
                ListSectionHeader(title: String(localized: "电脑"))
                VStack(spacing: 0) {
                    ForEach(Array(all.enumerated()), id: \.element.id) { index, machine in
                        if index > 0 {
                            Rectangle().fill(colors.hairline).frame(height: 1)
                        }
                        machineRow(machine, first: index == 0, last: index == all.count - 1)
                    }
                }
                .background(colors.card, in: .group)
                .overlay(RoundedRectangle.group.strokeBorder(colors.hairline, lineWidth: 1).allowsHitTesting(false))
                .plainListRow()
            }
        }

        /// 电脑组里的一行。整组只占列表的一行，没法左滑，改名和删除靠长按菜单和读屏的操作。
        private func machineRow(_ machine: MachineRecord, first: Bool, last: Bool) -> some View {
            let open = { app.path = [.machine(machine.id)] }
            let rename = { machines.beginRename(machine.id) }
            let delete = { machines.deleteTarget = machine.id }
            let radius = CornerRadius.group
            return MachineRow(
                machine: machine, list: app.sessionList(for: machine.id), onOpen: open, onRename: rename,
                onDelete: delete
            )
            .contentShape(
                .contextMenuPreview,
                UnevenRoundedRectangle(
                    topLeadingRadius: first ? radius : 0, bottomLeadingRadius: last ? radius : 0,
                    bottomTrailingRadius: last ? radius : 0, topTrailingRadius: first ? radius : 0, style: .continuous)
            )
            .contextMenu {
                Button("打开", systemImage: "terminal", action: open)
                Button("改名", systemImage: "pencil", action: rename)
                Button("删除", systemImage: "trash", role: .destructive, action: delete)
            }
            // 确认框挂在这一行上：iOS 26 起它是指向所挂视图的气泡，挂在整页上会指到屏幕中间。
            .confirmationDialog(
                "删除这台电脑？", isPresented: confirmsDelete(machine.id), titleVisibility: .visible
            ) {
                Button("删除", role: .destructive) { Task { await machines.delete(machine.id) } }
            } message: {
                // 连着时删除前先请电脑解除配对（`AppModel.leave`）；没连着时等它再连上。
                if app.sessionList(for: machine.id)?.linkState.isConnected == true {
                    Text("会删掉这部手机上为它保存的设备密钥，以后要重新扫码配对；这台电脑上的配对记录也一起撤销。")
                } else {
                    Text("会删掉这部手机上为它保存的设备密钥，以后要重新扫码配对。\n现在没连着这台电脑：等它下次连上再撤销它上面的配对记录，在那之前它的 agent 等回答时可能还会推送到这部手机。也可以在那台电脑上 Runode 的「配对设备」页里撤销。")
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
                    // 连上以后才有会话实时的状态；没连上时只写电脑的名字。
                    let live = app.sessionList(for: resume.machine.id)?.session(resume.session)
                    Button(action: app.resumeRecent) {
                        ResumeCard(
                            item: resume, mark: live.map { StatusMark(SessionStatus.of($0)) } ?? .idle, live: live)
                    }
                    .buttonStyle(.plain)
                    .plainListRow()
                }
            }
        }

        /// 钉在页面底下的配对和新开终端两个按钮。
        private var actionBar: some View {
            let spawnable = app.spawnableLists
            let targets = spawnTargets
            let busy = spawnable.contains(where: \.isSpawning)
            return tileRow {
                Button {
                    app.startPairing()
                } label: {
                    HStack(spacing: 8) {
                        Image(systemName: "qrcode.viewfinder")
                            .font(.system(size: pairIconSize))
                        Text("配对电脑")
                    }
                    .frame(maxWidth: .infinity)
                }
                .buttonStyle(SurfaceButtonStyle(.secondary, height: 50, cornerRadius: CornerRadius.action))
                if targets.count == 1, let target = targets.first, target.list.agents.isEmpty {
                    Button {
                        spawn(at: target)
                    } label: {
                        spawnLabel(busy: busy)
                    }
                    .buttonStyle(SurfaceButtonStyle(.primary, height: 50, cornerRadius: CornerRadius.action))
                    .disabled(busy)
                } else {
                    Menu {
                        spawnMenu(targets)
                    } label: {
                        spawnLabel(busy: busy)
                            .actionSurface(.primary, height: 50, cornerRadius: CornerRadius.action)
                    }
                    .buttonStyle(.plain)
                    .disabled(busy || spawnable.isEmpty)
                }
            }
            .font(.subheadline)
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
            .background(colors.page)
        }

        private func spawnLabel(busy: Bool) -> some View {
            HStack(spacing: 8) {
                if busy {
                    // 实心的主按钮上字用页面的底色，转圈跟着。
                    ProgressView()
                        .tint(colors.page)
                } else {
                    Image(systemName: "plus")
                        .font(.system(size: spawnIconSize, weight: .bold))
                }
                Text("新开终端")
            }
            .frame(maxWidth: .infinity)
        }

        /// 新开会话能开在哪：有 `spawnChoices` 的电脑一个工作区一项，让用户挑；别的电脑一台一项，开在电脑上
        /// 当前的工作区里。连着的电脑不止一台时每项带上电脑的名字，电脑开着几个窗口时带上窗口，同名的
        /// 工作区才分得清。
        private struct SpawnTarget: Identifiable {
            let id: String
            let title: String
            let list: SessionListModel
            let section: SessionSection?
        }

        private var spawnTargets: [SpawnTarget] {
            let lists = app.spawnableLists
            return lists.flatMap { list -> [SpawnTarget] in
                let machine = list.machine.name
                let choices = list.spawnChoices
                guard !choices.isEmpty else {
                    return [SpawnTarget(id: "\(list.machine.id)", title: machine, list: list, section: nil)]
                }
                return choices.map { section in
                    var name = Presentation.sectionTitle(section)
                    if let window = section.window { name = String(localized: "\(name)，窗口 \(window)") }
                    return SpawnTarget(
                        id: "\(list.machine.id)/\(section.id)",
                        title: lists.count > 1 ? String(localized: "\(machine)，\(name)") : name,
                        list: list, section: section)
                }
            }
        }

        /// 新开终端的菜单：只有一处能开时直接是开什么（终端或那台电脑上装着的 agent），否则先挑开在哪，
        /// 那台电脑上装着 agent 时再挑开什么。
        @ViewBuilder
        private func spawnMenu(_ targets: [SpawnTarget]) -> some View {
            if targets.count == 1, let target = targets.first {
                SpawnKindItems(agents: target.list.agents) { agent in spawn(at: target, agent: agent) }
            } else {
                let title =
                    targets.contains { $0.section != nil }
                    ? String(localized: "在哪里新开终端？") : String(localized: "在哪台电脑上新开终端？")
                Section(title) {
                    ForEach(targets) { target in
                        if target.list.agents.isEmpty {
                            Button(target.title) { spawn(at: target) }
                        } else {
                            Menu(target.title) {
                                SpawnKindItems(agents: target.list.agents) { agent in spawn(at: target, agent: agent) }
                            }
                        }
                    }
                }
            }
        }

        private func spawn(at target: SpawnTarget, agent: InstalledAgent? = nil) {
            let list = target.list
            if let pageSize {
                list.spawnSize = TerminalView.gridSize(
                    fitting: pageSize, scale: displayScale, contentSize: UIContentSizeCategory(dynamicTypeSize),
                    fontSize: app.settings.preferences.fontSize.map { CGFloat($0) })
            }
            Task {
                if let section = target.section {
                    await list.spawn(in: section, agent: agent)
                } else {
                    await list.spawn(agent: agent)
                }
            }
        }

        // MARK: 出错

        /// 首页上新开会话出的错。首页被别的页盖住时不弹，留给那一页自己的提示。
        private var spawnErrorList: SessionListModel? {
            guard app.path.isEmpty else { return nil }
            return app.machineLists.first { $0.errorMessage != nil }
        }

        /// 电脑列表出的错；设置页盖着首页时留给设置页自己弹。
        private var machineError: String? {
            app.showingSettings ? nil : machines.errorMessage
        }

        private var errorMessage: String? {
            machineError ?? spawnErrorList?.errorMessage
        }

        private var showsError: Binding<Bool> {
            Binding(
                get: { errorMessage != nil },
                set: { shown in
                    guard !shown else { return }
                    if machineError != nil {
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

    /// 首页顶上的一句话（`Presentation.homeTitle`），有会话在等你时后面闪着光标；下面一行补充
    /// （`Presentation.homeDetail`），说的是在干活的会话时前面带盲文格。
    private struct HomeHeadline: View {
        let summary: HomeSummary
        @ScaledMetric(relativeTo: .subheadline) private var glyphSize: CGFloat = 15

        var body: some View {
            VStack(alignment: .leading, spacing: 5) {
                HStack(alignment: .firstTextBaseline, spacing: 7) {
                    Text(Presentation.homeTitle(summary))
                        .font(.title.weight(.semibold))
                        .monospacedDigit()
                    if summary.waiting > 0 {
                        BlinkingCursor()
                            // 光标底边比基线低一截，和汉字的下沿齐。
                            .alignmentGuide(.firstTextBaseline) { d in d.height * 11 / 13 }
                    }
                }
                if let detail = Presentation.homeDetail(summary) {
                    HStack(spacing: 7) {
                        if summary.working > 0 {
                            StatusGlyph(.working, size: glyphSize)
                        }
                        Text(detail)
                            .font(.subheadline)
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .padding(.horizontal, 2)
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)
        }
    }

    /// 等你回答的会话不止一个时，摊开的那张卡底下露出的一截卡边，表示后面还有。只是装饰。
    private struct StackEdge: View {
        @Environment(\.themeColors) private var colors

        var body: some View {
            let shape = UnevenRoundedRectangle(
                bottomLeadingRadius: CornerRadius.group, bottomTrailingRadius: CornerRadius.group, style: .continuous)
            // 上沿压在卡片底下，描边只露出左右和下面三条。
            shape.fill(colors.stackEdge)
                .overlay(shape.strokeBorder(colors.hairline, lineWidth: 1))
                .frame(height: 20)
                .padding(.top, -10)
                .padding(.horizontal, 16)
                .accessibilityHidden(true)
        }
    }

    /// 电脑那一行前面的标记：等你回答、在干活是对应的 `StatusGlyph`，连不上是叉，连着而只有普通会话时
    /// 是提示符，没连上时不画。和后面第一行字的中线对齐。
    private struct MachineLineMark: View {
        let tone: MachineLine.Tone
        let connected: Bool
        @ScaledMetric(relativeTo: .footnote) private var size: CGFloat = 12
        @Environment(\.themeColors) private var colors

        var body: some View {
            Group {
                switch tone {
                case .waiting: StatusGlyph(.waiting, size: size)
                case .working: StatusGlyph(.working, size: size)
                case .failure:
                    Image(systemName: "xmark")
                        .font(.system(size: size * 0.8, weight: .bold))
                        .foregroundStyle(colors.failure)
                        .frame(width: size, height: size)
                case .neutral:
                    if connected { StatusGlyph(.idle, size: size) }
                }
            }
            // footnote 的大写字母高约为标记边长的四分之三，中线在基线上方半个字高。
            .alignmentGuide(.firstTextBaseline) { [size] d in d[VerticalAlignment.center] + size * 0.375 }
            .accessibilityHidden(true)
        }
    }

    /// 电脑组里的一台电脑：名字，下面是连接状态和会话数，点了打开它的会话列表。断开了在等重连或者连不上时，
    /// 右边是立刻重连的「重试」，和打开列表的那块并排（列表里按钮套按钮点不了里面那个）。
    private struct MachineRow: View {
        let machine: MachineRecord
        let list: SessionListModel?
        let onOpen: () -> Void
        let onRename: () -> Void
        let onDelete: () -> Void
        @ScaledMetric(relativeTo: .body) private var iconSize: CGFloat = 20
        @Environment(\.themeColors) private var colors

        var body: some View {
            let state = list?.linkState ?? .idle
            let retries = Self.offersRetry(state)
            HStack(spacing: 0) {
                Button(action: onOpen) {
                    HStack(spacing: 12) {
                        Image(systemName: "laptopcomputer")
                            .resizable()
                            .scaledToFit()
                            .frame(width: iconSize, height: iconSize)
                            .foregroundStyle(state.isConnected ? HierarchicalShapeStyle.primary : .tertiary)
                            .accessibilityHidden(true)
                        VStack(alignment: .leading, spacing: 1) {
                            Text(machine.name)
                                .font(.subheadline.weight(.semibold))
                                .foregroundStyle(state.isConnected ? HierarchicalShapeStyle.primary : .secondary)
                                .lineLimit(2)
                            LinkClock(state: state) { now in
                                let line = Presentation.machineLine(
                                    state: state, sessions: list?.sessions ?? [], loaded: list?.loaded ?? false,
                                    now: now)
                                HStack(alignment: .firstTextBaseline, spacing: 6) {
                                    MachineLineMark(tone: line.tone, connected: state.isConnected)
                                    Text(attributed(line))
                                }
                            }
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                        }
                        Spacer(minLength: 0)
                        if !retries {
                            DisclosureChevron()
                        }
                    }
                    .padding(.leading, 14)
                    .padding(.trailing, retries ? 12 : 14)
                    .padding(.vertical, retries ? 6 : 8)
                    .frame(maxWidth: .infinity, minHeight: 56, alignment: .leading)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityHint("打开这台电脑上的会话")
                .accessibilityActions {
                    Button("改名", action: onRename)
                    Button("删除", action: onDelete)
                }
                if retries {
                    Button {
                        list?.reconnect()
                    } label: {
                        Text("重试")
                            .font(.subheadline.weight(.medium))
                            .padding(.horizontal, 16)
                    }
                    .buttonStyle(SurfaceButtonStyle(.raised))
                    .padding(.trailing, 6)
                    .accessibilityHint(Text("重新连接 \(machine.name)"))
                }
            }
        }

        /// 断开了在等重连、或者连不上不再自动重试时，给用户一个立刻重连的按钮。
        private static func offersRetry(_ state: LinkState) -> Bool {
            switch state {
            case .waiting, .failed: true
            case .idle, .connecting, .connected: false
            }
        }

        private func attributed(_ line: MachineLine) -> AttributedString {
            var result = AttributedString()
            if let emphasis = line.emphasis {
                var head = AttributedString(emphasis)
                switch line.tone {
                case .waiting: head.swiftUI.foregroundColor = colors.waiting
                case .working: head.swiftUI.foregroundColor = colors.working
                case .failure: head.swiftUI.foregroundColor = colors.failure
                case .neutral: break
                }
                result += head
            }
            result += AttributedString(line.text)
            return result
        }
    }

    /// 上次打开的终端，样子和电脑组里的一行一样：前面是会话状态的标记，下面是会话在干什么和电脑名。
    private struct ResumeCard: View {
        let item: ResumeItem
        let mark: StatusMark
        /// 会话实时的状态，下面那行由 `Presentation.sessionSubtitle` 说；还没连上、不知道时为空。
        let live: SessionInfo?
        @ScaledMetric(relativeTo: .body) private var iconSize: CGFloat = 20
        @ScaledMetric(relativeTo: .body) private var glyphSize: CGFloat = 16
        @Environment(\.themeColors) private var colors

        var body: some View {
            HStack(spacing: 12) {
                StatusGlyph(mark, size: glyphSize)
                    .frame(width: iconSize)
                VStack(alignment: .leading, spacing: 1) {
                    Text(item.title)
                        .font(.subheadline.weight(.semibold))
                        .lineLimit(1)
                    subtitle
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 8)
                DisclosureChevron()
            }
            .padding(.vertical, 8)
            .padding(.horizontal, 14)
            .frame(maxWidth: .infinity, minHeight: 56, alignment: .leading)
            .background(colors.card, in: .group)
            .overlay(RoundedRectangle.group.strokeBorder(colors.hairline, lineWidth: 1).allowsHitTesting(false))
            .contentShape(.group)
            .accessibilityElement(children: .combine)
            .accessibilityHint("打开这个终端")
        }

        @ViewBuilder private var subtitle: some View {
            if let live {
                TimelineView(AgentClockSchedule(live)) { context in
                    SessionSubtitleLine(session: live, trailingMachine: item.machine.name, now: context.date)
                }
            } else {
                Text(item.machine.name)
            }
        }
    }

    /// 等你回答的一个会话：头部是标题和状态，不止一个在等时右边的「1 / 2」翻到下一张；中间是屏幕的摘录，
    /// 点头部或摘录打开终端；下面直接快速回复。
    private struct WaitingCard: View {
        let item: WaitingSession
        let showsMachine: Bool
        /// 这张是等你回答的第几个，从 1 数。
        let position: Int
        let count: Int
        let onOpen: () -> Void
        let onNext: () -> Void
        let onPrevious: () -> Void
        @Environment(\.themeColors) private var colors

        var body: some View {
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 10) {
                    header
                    if count > 1 {
                        pageButton
                    }
                }
                .frame(minHeight: 44)
                // 认出了提问时选项单独列在下面，摘录只放问题。
                let prompt = item.list.prompts[item.session.id]
                let preview = prompt?.question ?? item.list.previews[item.session.id] ?? []
                if !preview.isEmpty {
                    Button(action: onOpen) {
                        HStack(spacing: 0) {
                            ScreenPreview(lines: preview, tone: .primary)
                            DisclosureChevron()
                                .padding(.trailing, 10)
                        }
                        // 和 `ScreenPreview` 自己的底同色，箭头看着在同一个底槽里。
                        .background(colors.well, in: .inner)
                        .contentShape(.inner)
                    }
                    .buttonStyle(.plain)
                    .accessibilityHint("打开这个终端")
                }
                QuickReplyBar(model: item.list.quickReply(for: item.session.id), prompt: prompt)
                    .accessibilityElement(children: .contain)
                    .accessibilityLabel("回复「\(Presentation.sessionTitle(item.session))」")
            }
        }

        /// 标题和状态。读屏时上下轻扫翻到别的等你回答的会话。
        @ViewBuilder
        private var header: some View {
            let button = Button(action: onOpen) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(Presentation.sessionTitle(item.session))
                        .font(.headline)
                        .lineLimit(2)
                    TimelineView(AgentClockSchedule(item.session)) { context in
                        SessionSubtitleLine(
                            session: item.session, machine: showsMachine ? item.list.machine.name : nil,
                            now: context.date
                        )
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityHint("打开这个终端")
            if count > 1 {
                button
                    .accessibilityValue(Text("第 \(position) 个，共 \(count) 个"))
                    .accessibilityAdjustableAction { direction in
                        switch direction {
                        case .increment: onNext()
                        case .decrement: onPrevious()
                        @unknown default: break
                        }
                    }
            } else {
                button
            }
        }

        /// 「1 / 2」：点了循环翻到下一张。
        private var pageButton: some View {
            Button(action: onNext) {
                Text(verbatim: "\(position) / \(count)")
                    .font(.caption.monospacedDigit())
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 10)
                    .frame(minHeight: 26)
                    .background(colors.raised, in: .capsule)
                    .frame(minWidth: 44, minHeight: 44)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("下一个等你的会话")
            .accessibilityValue(Text("第 \(position) 个，共 \(count) 个"))
        }
    }
#endif
