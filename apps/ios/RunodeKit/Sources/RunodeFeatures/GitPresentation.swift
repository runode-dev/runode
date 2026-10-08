import Foundation
import RunodeProtocol

/// Git 页上给人看的文字。
extension Presentation {
    /// 文件状态的单字母标记，和电脑上 Git 面板的一样。
    public static func gitLetter(_ status: GitFileStatus) -> String {
        switch status {
        case .modified: "M"
        case .added: "A"
        case .deleted: "D"
        case .renamed: "R"
        case .untracked: "U"
        case .conflicted: "!"
        case .unknown: "?"
        }
    }

    /// 文件状态给 VoiceOver 读的名字。
    public static func gitStatusName(_ status: GitFileStatus) -> String {
        switch status {
        case .modified: String(localized: "已修改")
        case .added: String(localized: "新增")
        case .deleted: String(localized: "已删除")
        case .renamed: String(localized: "git.status.renamed", defaultValue: "改名")
        case .untracked: String(localized: "未跟踪")
        case .conflicted: String(localized: "有冲突")
        case .unknown(let text): text
        }
    }

    /// 文件名和它所在的目录（相对仓库根，根目录下的为空）。
    public static func gitPathParts(_ path: String) -> (name: String, directory: String?) {
        guard let slash = path.lastIndex(of: "/") else { return (path, nil) }
        return (String(path[path.index(after: slash)...]), String(path[..<slash]))
    }

    /// 分支那一行：分支名，分离头指针时写短哈希，还没有提交时写「还没有提交」。
    public static func gitBranch(_ status: GitStatus) -> String {
        if let branch = status.branch { return branch }
        if let head = status.head { return String(localized: "分离于 \(head)") }
        return String(localized: "还没有提交")
    }

    /// 和上游差几个提交：`origin/main · ↑2 ↓1`、`origin/main · 已同步`；没有上游时说明能不能推。
    public static func gitUpstream(_ status: GitStatus) -> String {
        guard let upstream = status.upstream else {
            return status.hasRemote ? String(localized: "还没有上游，推送时会设好") : String(localized: "没有配置远端")
        }
        var counts: [String] = []
        if status.ahead > 0 { counts.append("↑\(status.ahead)") }
        if status.behind > 0 { counts.append("↓\(status.behind)") }
        return "\(upstream) · \(counts.isEmpty ? String(localized: "已同步") : counts.joined(separator: " "))"
    }

    /// 做到一半的操作的提醒；没有时为空。
    public static func gitOperation(_ operation: GitOperation?) -> String? {
        let name: String
        switch operation {
        case nil: return nil
        case .merge: name = String(localized: "合并")
        case .rebase: name = String(localized: "变基")
        case .cherryPick: name = String(localized: "拣选")
        case .revert: name = String(localized: "撤销提交")
        case .unknown(let text): name = text
        }
        return String(localized: "\(name)做到一半，解决冲突后在电脑上继续或放弃")
    }

    /// 改仓库的操作的名字。
    public static func gitAction(_ action: GitModel.Action) -> String {
        switch action {
        case .stage: String(localized: "暂存")
        case .unstage: String(localized: "取消暂存")
        case .discard: String(localized: "丢弃改动")
        case .commit: String(localized: "提交")
        case .fetch: String(localized: "获取")
        case .pull: String(localized: "拉取")
        case .push: String(localized: "推送")
        case .sync: String(localized: "同步")
        case .checkout: String(localized: "切换分支")
        }
    }

    /// 正在办的操作，转圈旁边的字。每个动作单写一句，别的语言里「正在」没法和动作名拼起来。
    public static func gitRunning(_ action: GitModel.Action) -> String {
        switch action {
        case .stage: String(localized: "正在暂存…")
        case .unstage: String(localized: "正在取消暂存…")
        case .discard: String(localized: "正在丢弃改动…")
        case .commit: String(localized: "正在提交…")
        case .fetch: String(localized: "正在获取…")
        case .pull: String(localized: "正在拉取…")
        case .push: String(localized: "正在推送…")
        case .sync: String(localized: "正在同步…")
        case .checkout: String(localized: "正在切换分支…")
        }
    }
}

/// Git 页以树形式查看时的一行。
public enum GitTreeItem: Hashable, Sendable {
    /// 目录：`path` 相对仓库根，并成一行的是链条最深的那个；`name` 是显示的名字（并成一行的写成
    /// `a/b/c`）；`files` 是它下面所有改动的文件，对整个目录暂存、取消暂存用。
    case directory(path: String, name: String, depth: Int, expanded: Bool, files: [GitFile])
    case file(GitFile, depth: Int)
}

extension Presentation {
    /// 把一段改动的文件按目录分层排成行，和电脑上 Git 面板的树形式一样（`file_tree`）：目录在前、
    /// 文件在后，各自按名字排（不分大小写）；只有一个子目录、没有文件的目录和子目录并成一行。
    /// `collapsed` 里的目录（并成一行的按最深的那个认）收起，下面的不排。
    public static func gitFileTree(_ files: [GitFile], collapsed: Set<String>) -> [GitTreeItem] {
        let root = GitTreeNode()
        for file in files {
            var parts = file.path.split(separator: "/").map(String.init)
            guard let name = parts.popLast() else { continue }
            var node = root
            for part in parts { node = node.child(part) }
            node.files.append((name, file))
        }
        var items: [GitTreeItem] = []
        root.push(dir: "", depth: 0, collapsed: collapsed, into: &items)
        return items
    }
}

private final class GitTreeNode {
    var dirs: [String: GitTreeNode] = [:]
    var files: [(name: String, file: GitFile)] = []

    func child(_ name: String) -> GitTreeNode {
        if let node = dirs[name] { return node }
        let node = GitTreeNode()
        dirs[name] = node
        return node
    }

    var allFiles: [GitFile] {
        files.map(\.file) + dirs.values.flatMap(\.allFiles)
    }

    func push(dir: String, depth: Int, collapsed: Set<String>, into items: inout [GitTreeItem]) {
        for name in dirs.keys.sorted(by: Self.byName) {
            var node = dirs[name]!
            var path = dir.isEmpty ? name : "\(dir)/\(name)"
            var label = name
            while node.files.isEmpty, node.dirs.count == 1, let (next, child) = node.dirs.first {
                path += "/\(next)"
                label += "/\(next)"
                node = child
            }
            let expanded = !collapsed.contains(path)
            items.append(.directory(path: path, name: label, depth: depth, expanded: expanded, files: node.allFiles))
            if expanded { node.push(dir: path, depth: depth + 1, collapsed: collapsed, into: &items) }
        }
        for entry in files.sorted(by: { Self.byName($0.name, $1.name) }) {
            items.append(.file(entry.file, depth: depth))
        }
    }

    private static func byName(_ a: String, _ b: String) -> Bool {
        let (la, lb) = (a.lowercased(), b.lowercased())
        return la == lb ? a < b : la < lb
    }
}
