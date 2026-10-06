import Foundation
import Observation
import RunodeConnection
import RunodeProtocol

/// 新建工作区时在手机上一级级浏览电脑上的目录：每进一个目录发一次 `ListDirs`，宿主回 `Dirs`，出错时回
/// 带着请求编号的 `Error`。只认最后一次请求的回话，点得快时前面的回话丢掉。选好以后由
/// `SessionListModel.createWorkspace(at:)` 去建。
@Observable
@MainActor
public final class DirectoryPickerModel {
    /// 现在列着的目录，宿主规范化过的绝对路径；第一次列好之前为空。
    public private(set) var path: String?
    /// `path` 里的子目录名，按名字排好。
    public private(set) var dirs: [String] = []
    /// 子目录太多，宿主只给了前面一部分。
    public private(set) var truncated = false
    public private(set) var isLoading = false
    public private(set) var errorMessage: String?
    /// 显示以 `.` 开头的目录。
    public var showsHidden = false

    @ObservationIgnored private let link: any HostLink
    @ObservationIgnored private var pending: UInt32?
    /// 在等回话的那次要列的目录，出错时重试用。
    @ObservationIgnored private var requested: String?

    public init(link: any HostLink) {
        self.link = link
    }

    /// 要显示的子目录：不显示隐藏目录时去掉以 `.` 开头的。
    public var visibleDirs: [String] {
        showsHidden ? dirs : dirs.filter { !$0.hasPrefix(".") }
    }

    /// 上一级目录；已经在根目录或者还没列好时为空。
    public var parent: String? {
        guard let path, path != "/" else { return nil }
        let parent = (path as NSString).deletingLastPathComponent
        return parent.isEmpty ? "/" : parent
    }

    /// 列一个目录，`path` 为空时是家目录。
    public func load(_ path: String?) async {
        let req = await link.nextRequestId()
        pending = req
        requested = path
        isLoading = true
        errorMessage = nil
        link.send(.listDirs(req: req, path: path))
    }

    /// 进到现在这个目录里名叫 `name` 的子目录。
    public func enter(_ name: String) async {
        guard let path else { return }
        await load((path as NSString).appendingPathComponent(name))
    }

    public func goUp() async {
        guard let parent else { return }
        await load(parent)
    }

    /// 收起出错的提示，回到上次列好的目录。
    public func dismissError() {
        errorMessage = nil
    }

    /// 上次没列成：再列一次同一个目录。
    public func retry() async {
        await load(requested)
    }

    /// 是给自己的回话时处理掉并返回真。
    func handle(_ message: HostMsg) -> Bool {
        switch message {
        case .dirs(let req, let path, let dirs, let truncated) where req == pending:
            pending = nil
            isLoading = false
            self.path = path
            self.dirs = dirs
            self.truncated = truncated
            return true
        case .error(let req?, _, let message) where req == pending:
            pending = nil
            isLoading = false
            errorMessage = message
            return true
        case .error(nil, _, HostMsg.unknownMessage) where pending != nil:
            // 电脑上的 runode 太旧，不认识 `ListDirs`，回的 `Error` 不带编号。
            pending = nil
            isLoading = false
            errorMessage = "电脑上的 runode 版本太旧，不能浏览目录，先升级它。"
            return true
        default:
            return false
        }
    }
}
