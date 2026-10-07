#if os(iOS)
    import RunodeConnection
    import SwiftUI

    /// 新建工作区时选电脑上的目录：从家目录开始，点子目录进去，「上一级」退出来，选好了点「在这里新建」。
    /// 同一页里换目录，不一层层推进导航栈，退出来也就不用一层层返回。电脑上的 app 没开着窗口时建不了
    /// 工作区，选好的目录里开的是后台终端，页脚说明这一点。
    struct DirectoryPickerView: View {
        @Bindable var picker: DirectoryPickerModel
        let hasDesktopWindow: Bool
        let onCancel: () -> Void
        let onChoose: (String) -> Void
        @Environment(\.themeColors) private var colors

        var body: some View {
            NavigationStack {
                List {
                    if let path = picker.path {
                        Section {
                            Label(Presentation.directory(path) ?? path, systemImage: "folder.fill")
                                .font(.headline)
                                .lineLimit(2)
                                .truncationMode(.head)
                                .accessibilityLabel("现在在 \(path)")
                        } footer: {
                            if hasDesktopWindow {
                                Text("新工作区的第一个终端开在这个目录里。")
                            } else {
                                Text("电脑上的 runode 没开着窗口，建不了工作区，会在这个目录里开一个后台终端。")
                            }
                        }
                        .themedRows()
                    }
                    Section {
                        if let parent = picker.parent {
                            Button {
                                Task { await picker.goUp() }
                            } label: {
                                Label("上一级", systemImage: "arrow.turn.left.up")
                            }
                            .accessibilityHint("回到 \(parent)")
                        }
                        ForEach(picker.visibleDirs, id: \.self) { name in
                            Button {
                                Task { await picker.enter(name) }
                            } label: {
                                HStack {
                                    Label(name, systemImage: "folder")
                                        .foregroundStyle(.primary)
                                    Spacer()
                                    DisclosureChevron()
                                }
                            }
                        }
                    } footer: {
                        if picker.truncated {
                            Text("子目录太多，只列出了前面一部分。")
                        }
                    }
                    .themedRows()
                }
                .themedForm()
                .disabled(picker.isLoading)
                .overlay {
                    if let message = picker.errorMessage {
                        ContentUnavailableView {
                            Label("列不出这个目录", systemImage: "exclamationmark.triangle")
                        } description: {
                            Text(message)
                        } actions: {
                            Button("重试") { Task { await picker.retry() } }
                            if picker.path != nil {
                                Button("回到上次的目录", action: picker.dismissError)
                            }
                        }
                        .background(colors.page)
                    } else if picker.isLoading, picker.path == nil {
                        ProgressView("正在读取目录…")
                    } else if !picker.isLoading, picker.path != nil, picker.visibleDirs.isEmpty {
                        ContentUnavailableView("没有子目录", systemImage: "folder")
                    }
                }
                .navigationTitle("新建工作区")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button("取消", action: onCancel)
                    }
                    ToolbarItem(placement: .primaryAction) {
                        Toggle(isOn: $picker.showsHidden) {
                            Label("显示隐藏目录", systemImage: picker.showsHidden ? "eye" : "eye.slash")
                        }
                        .toggleStyle(.button)
                    }
                    ToolbarItem(placement: .bottomBar) {
                        Button {
                            if let path = picker.path { onChoose(path) }
                        } label: {
                            Text("在这里新建").frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.borderedProminent)
                        .disabled(picker.path == nil || picker.isLoading)
                    }
                }
            }
        }
    }
#endif
