//! 分屏布局树：拆分、关闭、改比例、均分，以及按方向找相邻的终端。

use runode_shared_types::pane::{Axis, Direction, Node, Rect, SplitId, neighbor};

/// 1 | (2 / 3)：左边一个，右边上下两个。
fn sample() -> Node<u32> {
    let mut root = Node::Leaf(1);
    assert!(root.split(1, 2, Axis::Horizontal, 10));
    assert!(root.split(2, 3, Axis::Vertical, 11));
    root
}

#[test]
fn split_inserts_after_the_target_in_reading_order() {
    let root = sample();
    assert_eq!(root.leaves(), [1, 2, 3]);
    assert!(!sample().split(9, 4, Axis::Vertical, 12));
}

#[test]
fn removing_a_leaf_promotes_its_sibling_and_focuses_the_nearest_leaf() {
    let mut root = sample();
    // 去掉左边的 1，焦点给右侧最靠前的 2，右侧那棵子树顶上来成为根。
    assert_eq!(root.remove(1), Some(2));
    assert_eq!(root.leaves(), [2, 3]);
    assert!(matches!(&root, Node::Split(split) if split.id == 11));

    let mut root = sample();
    assert_eq!(root.remove(3), Some(2));
    assert_eq!(root.leaves(), [1, 2]);

    let mut single = Node::Leaf(1);
    assert_eq!(single.remove(1), None);
    assert_eq!(single, Node::Leaf(1));
}

#[test]
fn resize_moves_the_nearest_divider_on_the_matching_axis() {
    let mut root = sample();
    let size = |id: SplitId| Some(if id == 10 { 1000. } else { 500. });
    // 3 往左挪：它自己所在的上下分屏方向不对，挪的是外层左右分隔线。
    assert!(root.resize(3, Direction::Left, 100., &size));
    let Node::Split(outer) = &root else { unreachable!() };
    assert!((outer.ratio - 0.4).abs() < 1e-6);
    // 3 往下：挪内层上下分隔线。
    assert!(root.resize(3, Direction::Down, 50., &size));
    let Node::Split(outer) = &root else { unreachable!() };
    let Node::Split(inner) = &*outer.second else { unreachable!() };
    assert!((inner.ratio - 0.6).abs() < 1e-6);
    // 只有一个终端时没有分隔线可挪。
    assert!(!Node::Leaf(1).resize(1, Direction::Left, 10., &size));
}

#[test]
fn equalize_gives_every_terminal_on_an_axis_the_same_share() {
    // 1 | 2 | 3：从 2 往右再分一次，外层要变成 1/3。
    let mut root = Node::Leaf(1);
    root.split(1, 2, Axis::Horizontal, 10);
    root.split(2, 3, Axis::Horizontal, 11);
    root.equalize();
    let Node::Split(outer) = &root else { unreachable!() };
    assert!((outer.ratio - 1. / 3.).abs() < 1e-6);
}

#[test]
fn neighbor_prefers_the_closest_overlapping_terminal() {
    let rect = |x, y, width, height| Rect { x, y, width, height };
    let panes = [(1, rect(0., 0., 500., 600.)), (2, rect(501., 0., 500., 300.)), (3, rect(501., 301., 500., 299.))];
    let from = |id| panes.iter().find(|(p, _)| *p == id).unwrap().1;
    let others = |id| panes.iter().copied().filter(move |(p, _)| *p != id);
    assert_eq!(neighbor(from(1), Direction::Right, others(1)), Some(2));
    assert_eq!(neighbor(from(3), Direction::Left, others(3)), Some(1));
    assert_eq!(neighbor(from(2), Direction::Down, others(2)), Some(3));
    assert_eq!(neighbor(from(2), Direction::Up, others(2)), None);
    assert_eq!(neighbor(from(1), Direction::Left, others(1)), None);
}

#[test]
fn rects_tile_the_area_by_the_ratios() {
    let rect = |x, y, width, height| Rect { x, y, width, height };
    let mut root = sample();
    root.set_ratio(10, 0.25);
    let rects = root.rects(rect(0., 0., 1000., 1000.));
    assert_eq!(
        rects,
        [(1, rect(0., 0., 250., 1000.)), (2, rect(250., 0., 750., 500.)), (3, rect(250., 500., 750., 500.))]
    );
    // 按算出来的矩形找方向上的邻居，和画出来的一样。
    let others = |id| rects.iter().copied().filter(move |(p, _)| *p != id);
    assert_eq!(neighbor(rects[0].1, Direction::Right, others(1)), Some(2));
    assert_eq!(neighbor(rects[2].1, Direction::Up, others(3)), Some(2));
    // 区域不从原点开始时整体平移。
    assert_eq!(Node::Leaf(7).rects(rect(10., 20., 30., 40.)), [(7, rect(10., 20., 30., 40.))]);
}
