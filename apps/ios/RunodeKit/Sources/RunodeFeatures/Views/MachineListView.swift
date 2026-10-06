#if os(iOS)
    import RunodeConnection
    import SwiftUI

    /// 配对过的 Mac。点进去看会话；左滑改名、删除。
    struct MachineListView: View {
        @Bindable var model: MachineListModel
        let onPair: () -> Void
        let onOpen: (UUID) -> Void

        var body: some View {
            List {
                ForEach(model.machines) { machine in
                    Button {
                        onOpen(machine.id)
                    } label: {
                        MachineRow(machine: machine)
                    }
                    .tint(.primary)
                    .swipeActions(edge: .trailing) {
                        Button("删除", systemImage: "trash", role: .destructive) {
                            model.deleteTarget = machine.id
                        }
                        Button("改名", systemImage: "pencil") {
                            model.beginRename(machine.id)
                        }
                        .tint(.orange)
                    }
                    .contextMenu {
                        Button("改名", systemImage: "pencil") { model.beginRename(machine.id) }
                        Button("删除", systemImage: "trash", role: .destructive) { model.deleteTarget = machine.id }
                    }
                }
            }
            .overlay {
                if model.loaded, model.machines.isEmpty {
                    ContentUnavailableView {
                        Label("还没有配对的 Mac", systemImage: "desktopcomputer")
                    } description: {
                        Text("在 Mac 上的 runode 里打开远程访问并生成配对二维码，再用这里扫码。")
                    } actions: {
                        Button("配对一台 Mac", action: onPair)
                            .buttonStyle(.borderedProminent)
                    }
                }
            }
            .navigationTitle("runode")
            .toolbar {
                ToolbarItem(placement: .primaryAction) {
                    Button("配对", systemImage: "qrcode.viewfinder", action: onPair)
                }
            }
            .task { await model.load() }
            // 按钮里用 `presenting` 带进来的编号：对话框关掉时绑定先被清掉，不能再从模型里读。
            .alert("改名", isPresented: $model.isRenaming, presenting: model.renameTarget) { id in
                TextField("名字", text: $model.renameText)
                Button("取消", role: .cancel) {}
                Button("保存") {
                    let name = model.renameText
                    Task { await model.rename(id, to: name) }
                }
            }
            .confirmationDialog(
                "删除这台 Mac？", isPresented: $model.isConfirmingDelete, titleVisibility: .visible,
                presenting: model.deleteTarget
            ) { id in
                Button("删除", role: .destructive) { Task { await model.delete(id) } }
            } message: { _ in
                Text("会删掉这部手机上为它保存的设备密钥，以后要重新扫码配对。Mac 上的配对记录请在 Mac 上撤销。")
            }
            .alert("出错了", isPresented: Binding(get: { model.errorMessage != nil }, set: { if !$0 { model.errorMessage = nil } })) {
                Button("好") {}
            } message: {
                Text(model.errorMessage ?? "")
            }
        }
    }

    private struct MachineRow: View {
        let machine: MachineRecord

        var body: some View {
            HStack(spacing: 12) {
                Image(systemName: "laptopcomputer")
                    .font(.title2)
                    .foregroundStyle(.tint)
                    .frame(width: 36)
                VStack(alignment: .leading, spacing: 2) {
                    Text(machine.name)
                        .font(.headline)
                    Text(machine.lastAddress.map { "上次连接 \($0)" } ?? machine.hostName)
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Image(systemName: "chevron.right")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(.tertiary)
            }
            .contentShape(Rectangle())
            .padding(.vertical, 4)
        }
    }
#endif
