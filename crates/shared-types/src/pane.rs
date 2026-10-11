//! 一个标签里的分屏布局：二叉树，叶子是终端，内部节点把空间按比例分给两边。
//!
//! 这里只有纯数据和几何计算，叶子用任意可比较的标识（实际是终端视图的实体 id），
//! 视图本身和界面由工作区管理。

/// 分屏的方向。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    /// 左右并排。
    Horizontal,
    /// 上下叠放。
    Vertical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    fn axis(self) -> Axis {
        match self {
            Direction::Left | Direction::Right => Axis::Horizontal,
            Direction::Up | Direction::Down => Axis::Vertical,
        }
    }

    /// 沿轴向前（右、下）为正。
    fn sign(self) -> f32 {
        match self {
            Direction::Left | Direction::Up => -1.,
            Direction::Right | Direction::Down => 1.,
        }
    }
}

/// 分屏节点的标识，用来在帧之间对应它的位置和拖动状态。
pub type SplitId = u64;

/// 两侧都不能比这个比例更窄。
const MIN_RATIO: f32 = 0.05;

#[derive(Clone, Debug, PartialEq)]
pub enum Node<T> {
    Leaf(T),
    Split(Split<T>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Split<T> {
    pub id: SplitId,
    pub axis: Axis,
    /// 前一半（左或上）占的比例。
    pub ratio: f32,
    pub first: Box<Node<T>>,
    pub second: Box<Node<T>>,
}

impl<T: Copy + PartialEq> Node<T> {
    /// 从左到右、从上到下的叶子顺序。
    pub fn leaves(&self) -> Vec<T> {
        let mut out = Vec::new();
        self.collect_leaves(&mut out);
        out
    }

    fn collect_leaves(&self, out: &mut Vec<T>) {
        match self {
            Node::Leaf(leaf) => out.push(*leaf),
            Node::Split(split) => {
                split.first.collect_leaves(out);
                split.second.collect_leaves(out);
            }
        }
    }

    pub fn is_leaf(&self) -> bool {
        matches!(self, Node::Leaf(_))
    }

    /// 把叶子 `target` 换成一个对半的分屏，`target` 在前（左或上），`new` 在后。
    /// 找不到 `target` 时返回 `false`。
    pub fn split(&mut self, target: T, new: T, axis: Axis, id: SplitId) -> bool {
        match self {
            Node::Leaf(leaf) if *leaf == target => {
                *self = Node::Split(Split {
                    id,
                    axis,
                    ratio: 0.5,
                    first: Box::new(Node::Leaf(target)),
                    second: Box::new(Node::Leaf(new)),
                });
                true
            }
            Node::Leaf(_) => false,
            Node::Split(split) => split.first.split(target, new, axis, id) || split.second.split(target, new, axis, id),
        }
    }

    /// 去掉叶子 `leaf`，它的兄弟顶替父节点。返回去掉之后该获得焦点的叶子：
    /// 兄弟一侧离它最近的那个。树里只剩这一个叶子或找不到它时什么都不做，返回 `None`。
    pub fn remove(&mut self, leaf: T) -> Option<T> {
        let Node::Split(split) = self else {
            return None;
        };
        let removed_first = *split.first == Node::Leaf(leaf);
        if removed_first || *split.second == Node::Leaf(leaf) {
            let sibling = if removed_first { &mut split.second } else { &mut split.first };
            let sibling = std::mem::replace(&mut **sibling, Node::Leaf(leaf));
            let leaves = sibling.leaves();
            let focus = if removed_first { leaves.first() } else { leaves.last() }.copied();
            *self = sibling;
            return focus;
        }
        split.first.remove(leaf).or_else(|| split.second.remove(leaf))
    }

    /// 沿 `direction` 挪动离 `leaf` 最近的、方向对得上的那条分隔线，挪 `pixels` 像素；
    /// `size_of` 给出分屏节点当前在该轴上的长度。没有这样的分隔线时返回 `false`。
    pub fn resize(
        &mut self,
        leaf: T,
        direction: Direction,
        pixels: f32,
        size_of: &impl Fn(SplitId) -> Option<f32>,
    ) -> bool {
        let Node::Split(split) = self else {
            return false;
        };
        let in_first = split.first.contains(leaf);
        if !in_first && !split.second.contains(leaf) {
            return false;
        }
        let child = if in_first { &mut split.first } else { &mut split.second };
        if child.resize(leaf, direction, pixels, size_of) {
            return true;
        }
        if split.axis != direction.axis() {
            return false;
        }
        let Some(size) = size_of(split.id).filter(|size| *size > 0.) else {
            return false;
        };
        split.ratio = (split.ratio + direction.sign() * pixels / size).clamp(MIN_RATIO, 1. - MIN_RATIO);
        true
    }

    pub fn set_ratio(&mut self, id: SplitId, ratio: f32) {
        if let Node::Split(split) = self {
            if split.id == id {
                split.ratio = ratio.clamp(MIN_RATIO, 1. - MIN_RATIO);
            } else {
                split.first.set_ratio(id, ratio);
                split.second.set_ratio(id, ratio);
            }
        }
    }

    /// 让同一方向上的各个终端一样大：每条分隔线按两侧在该方向上的终端数分配。
    pub fn equalize(&mut self) {
        if let Node::Split(split) = self {
            split.first.equalize();
            split.second.equalize();
            let first = split.first.weight(split.axis);
            let second = split.second.weight(split.axis);
            split.ratio = first / (first + second);
        }
    }

    /// 沿 `axis` 排成一列的终端数。
    fn weight(&self, axis: Axis) -> f32 {
        match self {
            Node::Split(split) if split.axis == axis => split.first.weight(axis) + split.second.weight(axis),
            _ => 1.,
        }
    }

    fn contains(&self, leaf: T) -> bool {
        match self {
            Node::Leaf(l) => *l == leaf,
            Node::Split(split) => split.first.contains(leaf) || split.second.contains(leaf),
        }
    }

    /// 整棵树铺满 `area` 时每个叶子占的矩形，按叶子顺序。只按比例切，不留分隔线的宽度，
    /// 所以没画出来的（比如后台标签里的）布局也算得出；相邻的两块正好接上。
    pub fn rects(&self, area: Rect) -> Vec<(T, Rect)> {
        let mut out = Vec::new();
        self.collect_rects(area, &mut out);
        out
    }

    fn collect_rects(&self, area: Rect, out: &mut Vec<(T, Rect)>) {
        match self {
            Node::Leaf(leaf) => out.push((*leaf, area)),
            Node::Split(split) => {
                let (first, second) = match split.axis {
                    Axis::Horizontal => {
                        let width = area.width * split.ratio;
                        (Rect { width, ..area }, Rect { x: area.x + width, width: area.width - width, ..area })
                    }
                    Axis::Vertical => {
                        let height = area.height * split.ratio;
                        (Rect { height, ..area }, Rect { y: area.y + height, height: area.height - height, ..area })
                    }
                };
                split.first.collect_rects(first, out);
                split.second.collect_rects(second, out);
            }
        }
    }
}

/// 屏幕上的矩形，用于按方向找相邻的终端。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    fn right(&self) -> f32 {
        self.x + self.width
    }

    fn bottom(&self) -> f32 {
        self.y + self.height
    }

    fn center(&self) -> (f32, f32) {
        (self.x + self.width / 2., self.y + self.height / 2.)
    }
}

/// 从 `from` 出发、在 `direction` 那一侧且和它有交叠的终端里离得最近的一个：
/// 先比沿方向的距离，再比垂直方向上中心的偏差。
pub fn neighbor<T: Copy>(
    from: Rect,
    direction: Direction,
    candidates: impl IntoIterator<Item = (T, Rect)>,
) -> Option<T> {
    // 分隔线有宽度，相邻终端之间会差一两个像素。
    const SLACK: f32 = 4.;
    let (cx, cy) = from.center();
    candidates
        .into_iter()
        .filter_map(|(leaf, rect)| {
            // 左右找时要上下有交叠，上下找时要左右有交叠。
            let rows_overlap = rect.y < from.bottom() && rect.bottom() > from.y;
            let columns_overlap = rect.x < from.right() && rect.right() > from.x;
            let (distance, overlaps, offset) = match direction {
                Direction::Left => (from.x - rect.right(), rows_overlap, rect.center().1 - cy),
                Direction::Right => (rect.x - from.right(), rows_overlap, rect.center().1 - cy),
                Direction::Up => (from.y - rect.bottom(), columns_overlap, rect.center().0 - cx),
                Direction::Down => (rect.y - from.bottom(), columns_overlap, rect.center().0 - cx),
            };
            (overlaps && distance > -SLACK).then_some((leaf, distance.max(0.), offset.abs()))
        })
        .min_by(|a, b| (a.1, a.2).partial_cmp(&(b.1, b.2)).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(leaf, ..)| leaf)
}
