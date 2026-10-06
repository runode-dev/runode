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
        case .modified: "已修改"
        case .added: "新增"
        case .deleted: "已删除"
        case .renamed: "改名"
        case .untracked: "未跟踪"
        case .conflicted: "有冲突"
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
        if let head = status.head { return "分离于 \(head)" }
        return "还没有提交"
    }

    /// 和上游差几个提交：`origin/main · ↑2 ↓1`、`origin/main · 已同步`；没有上游时说明能不能推。
    public static func gitUpstream(_ status: GitStatus) -> String {
        guard let upstream = status.upstream else {
            return status.hasRemote ? "还没有上游，推送时会设好" : "没有配置远端"
        }
        var counts: [String] = []
        if status.ahead > 0 { counts.append("↑\(status.ahead)") }
        if status.behind > 0 { counts.append("↓\(status.behind)") }
        return "\(upstream) · \(counts.isEmpty ? "已同步" : counts.joined(separator: " "))"
    }

    /// 做到一半的操作的提醒；没有时为空。
    public static func gitOperation(_ operation: GitOperation?) -> String? {
        let name: String
        switch operation {
        case nil: return nil
        case .merge: name = "合并"
        case .rebase: name = "变基"
        case .cherryPick: name = "拣选"
        case .revert: name = "撤销提交"
        case .unknown(let text): name = text
        }
        return "\(name)做到一半，解决冲突后在电脑上继续或放弃"
    }

    /// 改仓库的操作的名字。
    public static func gitAction(_ action: GitModel.Action) -> String {
        switch action {
        case .stage: "暂存"
        case .unstage: "取消暂存"
        case .commit: "提交"
        case .fetch: "获取"
        case .pull: "拉取"
        case .push: "推送"
        case .sync: "同步"
        case .checkout: "切换分支"
        }
    }

    /// 正在办的操作，转圈旁边的字。
    public static func gitRunning(_ action: GitModel.Action) -> String {
        "正在\(gitAction(action))…"
    }
}
