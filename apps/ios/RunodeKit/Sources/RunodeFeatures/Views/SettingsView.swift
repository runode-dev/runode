#if os(iOS)
    import RunodeConnection
    import SwiftUI

    /// 设置页：终端的默认尺寸方式、字号、响铃震动，灵动岛上的 agent 状态，配对过的电脑，报给电脑的设备名，
    /// 以及版本和开源许可。开关、选择器要系统的行样式，所以用分组的 `Form`，不用首页那种自己画的卡片列表。改了马上
    /// 生效、马上存（见 `SettingsModel`）。
    struct SettingsView: View {
        let app: AppModel
        @Bindable var settings: SettingsModel
        @Bindable var machines: MachineListModel
        @Environment(\.dismiss) private var dismiss

        var body: some View {
            NavigationStack {
                Form {
                    terminalSection
                    agentActivitySection
                    machineSection
                    deviceSection
                    aboutSection
                }
                .themedForm()
                .navigationTitle("设置")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) {
                        Button("完成") { dismiss() }
                    }
                }
                .navigationDestination(for: UUID.self) { id in
                    MachineSettingsView(machines: machines, id: id)
                }
            }
        }

        // MARK: 终端

        private var terminalSection: some View {
            Section {
                Picker("默认尺寸", selection: $settings.preferences.defaultSize) {
                    ForEach(SizePreference.allCases, id: \.self) { preference in
                        Text(Presentation.sizePreference(preference)).tag(preference)
                    }
                }
                // 主题的强调色是前景色，开关打开时底和圆钮一样白，看不出开着，开关用系统的绿色。
                Toggle("字号跟随系统", isOn: followsSystemFont)
                    .tint(.green)
                if let size = settings.preferences.fontSize {
                    Stepper(value: fontSize, in: AppPreferences.fontSizes, step: 1) {
                        LabeledContent("字号", value: "\(Int(size)) 点")
                    }
                    Text("ls -la ~/projects")
                        .font(.system(size: size, design: .monospaced))
                        .lineLimit(1)
                        .minimumScaleFactor(0.5)
                        .accessibilityLabel("字号预览")
                }
                Toggle("响铃时震动", isOn: $settings.preferences.bellHaptics)
                    .tint(.green)
            } header: {
                Text("终端")
            } footer: {
                Text("默认尺寸用在新打开的终端上，终端页右上角还能临时换。「自动」在电脑上没有窗口显示这个终端时适配手机，有时跟随电脑。")
            }
            .themedRows()
        }

        private var followsSystemFont: Binding<Bool> {
            Binding(
                get: { settings.preferences.fontSize == nil },
                set: { follows in
                    // 关掉跟随时从系统现在给的字号开始调。
                    settings.setFontSize(follows ? nil : Double(TerminalViewFontDefaults.systemBase))
                })
        }

        private var fontSize: Binding<Double> {
            Binding(
                get: { settings.preferences.fontSize ?? Double(TerminalViewFontDefaults.systemBase) },
                set: { settings.setFontSize($0) })
        }

        // MARK: 灵动岛

        private var agentActivitySection: some View {
            Section {
                Toggle("在灵动岛显示 agent 状态", isOn: $settings.preferences.showsAgentActivity)
                    .tint(.green)
            } header: {
                Text("灵动岛")
            } footer: {
                Text("有 agent 的时候，在灵动岛和锁屏上显示各台电脑上有几个在干活、几个在等你回答。离开 Runode 一会儿后连接会断开，那时显示的是断开前的样子，打开 Runode 才会刷新。")
            }
            .themedRows()
        }

        // MARK: 电脑

        private var machineSection: some View {
            Section("电脑") {
                ForEach(machines.machines) { machine in
                    NavigationLink(value: machine.id) {
                        MachineRow(machine: machine, state: app.sessionList(for: machine.id)?.linkState ?? .idle)
                    }
                }
                Button {
                    app.pairsAfterSettings = true
                    dismiss()
                } label: {
                    Label("配对新电脑", systemImage: "qrcode.viewfinder")
                }
            }
            .themedRows()
        }

        // MARK: 本机

        private var deviceSection: some View {
            Section {
                DeviceNameField(settings: settings)
            } header: {
                Text("这部手机")
            } footer: {
                Text("电脑上提到这部手机时用这个名字，比如终端尺寸由谁决定。下次连上电脑时生效；电脑上已配对设备列表里的名字是配对时的，重新配对后才会更新。留空用系统的名字。")
            }
            .themedRows()
        }

        // MARK: 关于

        private var aboutSection: some View {
            Section {
                LabeledContent("版本", value: Self.version)
            } header: {
                Text("关于")
            }
            .themedRows()
        }

        /// 「0.1.0（1）」：营销版本号和构建号。
        static var version: String {
            let info = Bundle.main.infoDictionary
            let short = info?["CFBundleShortVersionString"] as? String ?? "—"
            guard let build = info?["CFBundleVersion"] as? String else { return short }
            return "\(short)（\(build)）"
        }
    }

    /// 设置里关掉「字号跟随系统」时的起点：按现在的动态字体算的终端默认字号。
    @MainActor
    private enum TerminalViewFontDefaults {
        static var systemBase: CGFloat {
            UIFontMetrics(forTextStyle: .body).scaledValue(for: 13).rounded()
        }
    }

    /// 设备名的输入框：打字时先放在本地，按完成或离开输入框时才存，免得每打一个字都存一次、都转给
    /// 各台电脑的连接。
    private struct DeviceNameField: View {
        @Bindable var settings: SettingsModel
        @State private var text = ""
        @FocusState private var focused: Bool

        var body: some View {
            TextField(settings.systemDeviceName, text: $text)
                .focused($focused)
                .submitLabel(.done)
                .autocorrectionDisabled()
                .onAppear { text = settings.preferences.deviceName ?? "" }
                .onSubmit(commit)
                .onChange(of: focused) { _, isFocused in
                    if !isFocused { commit() }
                }
                .onDisappear(perform: commit)
                .accessibilityLabel("设备名")
        }

        private func commit() {
            settings.setDeviceName(text)
            text = settings.preferences.deviceName ?? ""
        }
    }

    /// 设置里的一台电脑：名字和连接状态。
    private struct MachineRow: View {
        let machine: MachineRecord
        let state: LinkState

        var body: some View {
            VStack(alignment: .leading, spacing: 2) {
                Text(machine.name)
                Text(Presentation.linkState(state))
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            .accessibilityElement(children: .combine)
        }
    }

    /// 一台电脑的详情：改名、看配对信息、删除。
    private struct MachineSettingsView: View {
        @Bindable var machines: MachineListModel
        let id: UUID
        @State private var name = ""
        @State private var confirmingDelete = false
        @Environment(\.dismiss) private var dismiss

        var body: some View {
            Group {
                if let machine = machines.machine(id) {
                    form(machine)
                } else {
                    ContentUnavailableView("这台电脑已经删掉了", systemImage: "laptopcomputer.slash")
                }
            }
            .themedForm()
            .navigationTitle(machines.machine(id)?.name ?? "电脑")
            .navigationBarTitleDisplayMode(.inline)
        }

        private func form(_ machine: MachineRecord) -> some View {
            Form {
                Section {
                    TextField(machine.hostName, text: $name)
                        .submitLabel(.done)
                        .autocorrectionDisabled()
                        .onAppear { name = machine.name }
                        .onSubmit { rename(machine) }
                        .onDisappear { rename(machine) }
                        .accessibilityLabel("名字")
                } header: {
                    Text("名字")
                } footer: {
                    Text("只改这部手机上显示的名字。")
                }
                .themedRows()
                Section("配对信息") {
                    LabeledContent("电脑名", value: machine.hostName)
                    LabeledContent("配对时间", value: machine.pairedAt.formatted(date: .abbreviated, time: .shortened))
                    if let address = machine.lastAddress {
                        LabeledContent("上次的地址", value: "\(address):\(machine.port)")
                    }
                    LabeledContent("设备编号") {
                        Text(machine.deviceId)
                            .font(.footnote.monospaced())
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .textSelection(.enabled)
                    }
                    LabeledContent("证书指纹") {
                        Text(machine.fingerprint.description)
                            .font(.footnote.monospaced())
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .textSelection(.enabled)
                    }
                }
                .themedRows()
                Section {
                    Button("删除这台电脑", role: .destructive) { confirmingDelete = true }
                        // 挂在按钮上，确认框的气泡才指着它。
                        .confirmationDialog("删除这台电脑？", isPresented: $confirmingDelete, titleVisibility: .visible) {
                            Button("删除", role: .destructive) {
                                Task {
                                    await machines.delete(id)
                                    dismiss()
                                }
                            }
                        }
                } footer: {
                    Text("会删掉这部手机上为它保存的设备密钥，以后要重新扫码配对。电脑上的配对记录要在那台电脑上撤销：运行 `runode remote revoke \(machine.deviceId.prefix(8))`。")
                }
                .themedRows()
            }
        }

        private func rename(_ machine: MachineRecord) {
            let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty, trimmed != machine.name else { return }
            Task { await machines.rename(id, to: trimmed) }
        }
    }
#endif
