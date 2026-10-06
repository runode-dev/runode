#if os(iOS)
    import RunodeConnection
    import RunodeProtocol
    import RunodeTerminal
    import SwiftUI
    import UIKit

    /// 首页：顶上是连着的 Mac 上加起来的统计，接着是等你回答的会话（带快速回复）、配对过的 Mac、
    /// 上次打开的终端和快捷操作。配对过的 Mac 在 App 处于前台时都连着（见 `AppModel`）。
    struct HomeView: View {
        @Bindable var app: AppModel
        /// 就是 `app.machineList`，单独拿着才能给改名、删除的对话框做绑定。
        @Bindable var machines: MachineListModel
        @Environment(\.displayScale) private var displayScale
        @Environment(\.dynamicTypeSize) private var dynamicTypeSize
        @State private var choosingSpawnMachine = false
        /// 新开会话用的网格尺寸，按首页的大小（和终端页差不多）算。
        @State private var spawnSize: GridSize?

        var body: some View {
            List {
                if !machines.machines.isEmpty {
                    summarySection
                    waitingSection
                    machineSection
                    resumeSection
                    actionSection
                }
            }
            .listStyle(.insetGrouped)
            .listRowSpacing(10)
            .overlay {
                if machines.loaded, machines.machines.isEmpty {
                    ContentUnavailableView {
                        Label("还没有配对的 Mac", systemImage: "laptopcomputer.and.iphone")
                    } description: {
                        Text("在 Mac 上打开远程访问，运行 `runode\u{00A0}remote\u{00A0}pair`，再用这里扫出现的二维码。配对页里有详细步骤。")
                    } actions: {
                        Button("配对一台 Mac") { app.startPairing() }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.large)
                    }
                }
            }
            .animation(.default, value: app.waitingSessions.map(\.id))
            .navigationTitle("runode")
            .task(id: machines.machines.map(\.id)) { await app.keepHomeRefreshing() }
            .refreshable { app.refreshHome() }
            .onGeometryChange(for: CGSize.self) { $0.size } action: { size in
                spawnSize = TerminalView.gridSize(
                    fitting: size, scale: displayScale, contentSize: UIContentSizeCategory(dynamicTypeSize))
            }
            .confirmationDialog("在哪台 Mac 上新开终端？", isPresented: $choosingSpawnMachine, titleVisibility: .visible) {
                ForEach(app.spawnableLists, id: \.machine.id) { list in
                    Button(list.machine.name) { spawn(on: list) }
                }
            }
            // 按钮里用 `presenting` 带进来的编号：对话框关掉时绑定先被清掉，不能再从模型里读。
            .alert("改名", isPresented: $machines.isRenaming, presenting: machines.renameTarget) { id in
                TextField("名字", text: $machines.renameText)
                Button("取消", role: .cancel) {}
                Button("保存") {
                    let name = machines.renameText
                    Task { await machines.rename(id, to: name) }
                }
            }
            .confirmationDialog(
                "删除这台 Mac？", isPresented: $machines.isConfirmingDelete, titleVisibility: .visible,
                presenting: machines.deleteTarget
            ) { id in
                Button("删除", role: .destructive) { Task { await machines.delete(id) } }
            } message: { _ in
                Text("会删掉这部手机上为它保存的设备密钥，以后要重新扫码配对。Mac 上的配对记录请在 Mac 上撤销。")
            }
            .alert("出错了", isPresented: showsError) {
                Button("好") {}
            } message: {
                Text(errorMessage ?? "")
            }
        }

        // MARK: 各块

        private var summarySection: some View {
            let summary = app.summary
            return Section {
                HStack(spacing: 10) {
                    StatTile(value: summary.waiting, title: "等你回答", tint: summary.waiting > 0 ? .orange : .primary)
                    StatTile(value: summary.working, title: "干活中", tint: summary.working > 0 ? .blue : .primary)
                    StatTile(value: summary.sessions, title: "会话", tint: .primary)
                }
                .listRowInsets(EdgeInsets())
                .listRowBackground(Color.clear)
            }
        }

        @ViewBuilder
        private var waitingSection: some View {
            let waiting = app.waitingSessions
            if !waiting.isEmpty {
                Section("等你回答") {
                    ForEach(waiting) { item in
                        WaitingCard(item: item, showsMachine: machines.machines.count > 1) {
                            app.openTerminal(machine: item.list.machine.id, session: item.session.id)
                        }
                    }
                }
            }
        }

        private var machineSection: some View {
            Section("Mac") {
                ForEach(machines.machines) { machine in
                    NavigationLink(value: Route.machine(machine.id)) {
                        MachineCard(machine: machine, list: app.sessionList(for: machine.id))
                    }
                    .swipeActions(edge: .trailing) {
                        Button("删除", systemImage: "trash", role: .destructive) {
                            machines.deleteTarget = machine.id
                        }
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
                }
            }
        }

        @ViewBuilder
        private var resumeSection: some View {
            if let resume = app.resume {
                Section("继续") {
                    Button(action: app.resumeRecent) {
                        ResumeCard(item: resume)
                    }
                    .buttonStyle(.plain)
                }
            }
        }

        private var actionSection: some View {
            let spawnable = app.spawnableLists
            return Section("快捷操作") {
                HStack(spacing: 10) {
                    ActionTile(title: "配对 Mac", systemImage: "qrcode.viewfinder", busy: false) {
                        app.startPairing()
                    }
                    ActionTile(title: "新开会话", systemImage: "plus", busy: spawnable.contains(where: \.isSpawning)) {
                        if spawnable.count == 1, let list = spawnable.first {
                            spawn(on: list)
                        } else {
                            choosingSpawnMachine = true
                        }
                    }
                    .disabled(spawnable.isEmpty)
                }
                .listRowInsets(EdgeInsets())
                .listRowBackground(Color.clear)
            }
        }

        private func spawn(on list: SessionListModel) {
            if let spawnSize { list.spawnSize = spawnSize }
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

    /// 卡片左边的图标：浅底圆角方块。
    private struct IconTile: View {
        let systemImage: String
        var tint: Color = .secondary

        var body: some View {
            Image(systemName: systemImage)
                .font(.title3)
                .foregroundStyle(tint)
                .frame(width: 40, height: 40)
                .background(Color(.tertiarySystemFill), in: RoundedRectangle(cornerRadius: 10))
                .accessibilityHidden(true)
        }
    }

    /// 统计那一排里的一格：大数字，下面一行说明。
    private struct StatTile: View {
        let value: Int
        let title: String
        let tint: Color

        var body: some View {
            VStack(alignment: .leading, spacing: 2) {
                Text(value, format: .number)
                    .font(.title2.weight(.bold).monospacedDigit())
                    .foregroundStyle(tint)
                    .contentTransition(.numericText(value: Double(value)))
                Text(title)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(12)
            .background(Color(.secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 12))
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
                    .background(Color(.tertiarySystemFill), in: RoundedRectangle(cornerRadius: 8))
                    Text(title)
                        .font(.subheadline.weight(.semibold))
                        .lineLimit(1)
                        .minimumScaleFactor(0.8)
                    Spacer(minLength: 0)
                }
                .padding(12)
                .frame(maxWidth: .infinity, minHeight: 56)
                .background(Color(.secondarySystemGroupedBackground), in: RoundedRectangle(cornerRadius: 12))
                .contentShape(RoundedRectangle(cornerRadius: 12))
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

    /// 一台 Mac：名字，下面是连接状态和会话数。
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
            }
            .padding(.vertical, 4)
            .accessibilityElement(children: .combine)
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
                Image(systemName: "chevron.right")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(.tertiary)
                    .accessibilityHidden(true)
            }
            .padding(.vertical, 4)
            .contentShape(Rectangle())
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
                            AgentBadge(agent: item.session.meta.agent)
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
                        Image(systemName: "chevron.right")
                            .font(.footnote.weight(.semibold))
                            .foregroundStyle(.tertiary)
                            .padding(.top, 4)
                            .accessibilityHidden(true)
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
            .padding(.vertical, 4)
        }

        /// 在哪台 Mac 的哪个目录：只配对了一台 Mac 时不写 Mac 的名字。
        private var place: String? {
            let parts = [showsMachine ? item.list.machine.name : nil, Presentation.directory(item.session.meta.cwd)]
                .compactMap { $0 }
            return parts.isEmpty ? nil : parts.joined(separator: " · ")
        }
    }
#endif
