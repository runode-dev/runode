//! 字符在终端里占几格。

use runode_terminal::cell_width;

#[test]
fn cell_widths() {
    assert_eq!(cell_width('a'), 1);
    assert_eq!(cell_width('中'), 2);
    assert_eq!(cell_width('\u{301}'), 0);
}
