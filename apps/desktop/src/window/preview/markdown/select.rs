//! 排版视图里的文字选择：位置记成（第几行、行里第几段文字、段里的字节偏移），整篇文档按这个顺序
//! 排；复制时按选中的范围拼出纯文本，列表带记号、引用带 `> `、表格的格子用制表符隔开。

use std::ops::Range;

use super::{Doc, Leaf, Nest, gap, marker_text};

/// 一段能选的文字：第几行、行里的第几段（表格的格子按先表头后各行、从左到右数，别的只有一段）。
pub(in crate::window::preview) type TextKey = (usize, usize);

/// 选区的一端。按字段的先后比大小就是在文档里的先后。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::window::preview) struct MdPos {
    pub row: usize,
    pub text: usize,
    /// 段里的字节偏移，落在字符边界上。
    pub offset: usize,
}

impl MdPos {
    pub fn key(self) -> TextKey {
        (self.row, self.text)
    }
}

/// 选区两端按先后排好。
pub(super) fn ordered((anchor, head): (MdPos, MdPos)) -> (MdPos, MdPos) {
    if anchor <= head { (anchor, head) } else { (head, anchor) }
}

/// 从 `start` 到 `end` 的选区里，长 `len` 的那段文字 `key` 选中了哪些字节；没选中时为空。
pub(super) fn selected_range(start: MdPos, end: MdPos, key: TextKey, len: usize) -> Option<Range<usize>> {
    if key < start.key() || key > end.key() {
        return None;
    }
    let from = if key == start.key() { start.offset.min(len) } else { 0 };
    let to = if key == end.key() { end.offset.min(len) } else { len };
    (from < to).then_some(from..to)
}

/// 整篇文档的选区：第一段文字的开头到最后一段的末尾；没有文字时为空。
pub(super) fn select_all(doc: &Doc) -> Option<(MdPos, MdPos)> {
    let first = doc.rows.iter().position(|row| !row.leaf.texts().is_empty())?;
    let last = doc.rows.iter().rposition(|row| !row.leaf.texts().is_empty())?;
    let texts = doc.rows[last].leaf.texts();
    let end = MdPos { row: last, text: texts.len() - 1, offset: texts.last().map_or(0, |text| text.len()) };
    Some((MdPos { row: first, ..MdPos::default() }, end))
}

/// 文件重读后，旧文档 `old` 里的位置 `pos` 在新文档 `new` 里还对不对得上：同一行同一段的文字一字
/// 不差。前面插了、删了块时行号对不上，选区就不留，免得高亮挪到别的文字上。
pub(super) fn still_valid(old: &Doc, new: &Doc, pos: MdPos) -> bool {
    let text =
        |doc: &Doc| doc.rows.get(pos.row).and_then(|row| row.leaf.texts().get(pos.text).map(|text| text.to_string()));
    text(old).is_some_and(|old| old.is_char_boundary(pos.offset) && Some(old) == text(new))
}

/// 选区 `start` 到 `end` 的纯文本。块之间空一行，同一个列表里的项、提示块的标题和正文之间只换行；
/// 从一行开头选起时带上列表记号（`- `、`1. `、`[x] `）和引用的 `> `；表格一行一行，格子之间是制表符。
pub(super) fn copy_text(doc: &Doc, start: MdPos, end: MdPos) -> String {
    let mut out = String::new();
    let mut prev: Option<usize> = None;
    for ix in start.row..=end.row.min(doc.rows.len().saturating_sub(1)) {
        let row = &doc.rows[ix];
        let texts = row.leaf.texts();
        let first = if ix == start.row { start.text } else { 0 };
        let last = if ix == end.row { end.text } else { texts.len().saturating_sub(1) };
        if texts.is_empty() || first > last || first >= texts.len() {
            continue;
        }
        // 选区停在下一块的开头：那一块一个字也没选上，分隔和记号也不要。
        if ix == end.row && ix != start.row && end.text == 0 && end.offset == 0 {
            break;
        }
        if let Some(prev) = prev {
            out.push_str(if (prev + 1..=ix).all(|ix| gap(&doc.rows, ix) < 0.75) { "\n" } else { "\n\n" });
        }
        if ix != start.row || (start.text == 0 && start.offset == 0) {
            out.push_str(&prefix(&row.nest));
        }
        let columns = match &row.leaf {
            Leaf::Table { aligns, .. } => aligns.len().max(1),
            _ => usize::MAX,
        };
        for (text_ix, text) in texts.iter().enumerate().take(last + 1).skip(first) {
            // 选区正好停在一个格子的开头时，那个格子不算选上。
            if text_ix > first && (ix, text_ix) == end.key() && end.offset == 0 {
                break;
            }
            if text_ix > first {
                out.push(if text_ix % columns == 0 { '\n' } else { '\t' });
            }
            let range = selected_range(start, end, (ix, text_ix), text.len()).unwrap_or(0..0);
            out.push_str(&text[range]);
        }
        prev = Some(ix);
    }
    out
}

/// 一行开头的引用记号和列表记号；列表项不是第一行时记号的位置空两格。
fn prefix(nest: &[Nest]) -> String {
    let mut depth = 0;
    nest.iter()
        .map(|level| match level {
            Nest::Quote { .. } => "> ".to_owned(),
            Nest::Item { marker, .. } => {
                depth += 1;
                marker.map_or_else(|| "  ".to_owned(), |marker| format!("{} ", marker_text(marker, depth)))
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;

    fn doc(text: &str) -> Doc {
        Doc::new(
            runode_preview::parse_markdown(text, &AtomicBool::new(false)).unwrap(),
            std::path::Path::new("/"),
            |_| None,
        )
    }

    fn pos(row: usize, text: usize, offset: usize) -> MdPos {
        MdPos { row, text, offset }
    }

    #[test]
    fn selected_ranges_cover_the_texts_between_the_ends() {
        let (start, end) = ordered((pos(3, 0, 2), pos(1, 1, 4)));
        assert_eq!((start, end), (pos(1, 1, 4), pos(3, 0, 2)));
        assert_eq!(selected_range(start, end, (1, 0), 10), None);
        assert_eq!(selected_range(start, end, (1, 1), 10), Some(4..10));
        assert_eq!(selected_range(start, end, (2, 5), 7), Some(0..7));
        assert_eq!(selected_range(start, end, (3, 0), 10), Some(0..2));
        assert_eq!(selected_range(start, end, (3, 1), 10), None);
        // 两端在同一处时什么也没选。
        assert_eq!(selected_range(start, start, (1, 1), 10), None);
    }

    #[test]
    fn copies_paragraphs_lists_and_quotes_as_plain_text() {
        let doc = doc("# Title\n\nsome *text* here\n\n- a\n- b\n  1. c\n\n> quoted\n");
        let (start, end) = select_all(&doc).unwrap();
        assert_eq!(copy_text(&doc, start, end), "Title\n\nsome text here\n\n- a\n- b\n  i. c\n\n> quoted");
        // 无序列表里的有序列表照 GitHub 用小写罗马数字。从段落中间选起：不带记号，只到选中的地方。
        assert_eq!(copy_text(&doc, pos(1, 0, 5), pos(3, 0, 1)), "text here\n\n- a\n- b");
    }

    /// 选区停在下一块的开头时只复制前面的块，不多出空行和记号。
    #[test]
    fn selections_ending_at_the_start_of_a_block_stop_before_it() {
        let paragraphs = doc("a\n\nb\n");
        assert_eq!(copy_text(&paragraphs, pos(0, 0, 0), pos(1, 0, 0)), "a");
        let list = doc("- a\n- b\n");
        assert_eq!(copy_text(&list, pos(0, 0, 0), pos(1, 0, 0)), "- a");
        let quote = doc("> a\n>\n> b\n");
        assert_eq!(copy_text(&quote, pos(0, 0, 0), pos(1, 0, 0)), "> a");
        // 停在同一块的开头之后照常带上那一点。
        assert_eq!(copy_text(&paragraphs, pos(0, 0, 0), pos(1, 0, 1)), "a\n\nb");
    }

    #[test]
    fn copies_tables_with_tabs_and_task_markers() {
        let doc = doc("| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n\n- [x] done\n- [ ] todo\n");
        let (start, end) = select_all(&doc).unwrap();
        assert_eq!(copy_text(&doc, start, end), "a\tb\n1\t2\n3\t4\n\n[x] done\n[ ] todo");
        // 从表格中间的格子选到另一个格子的一半。
        assert_eq!(copy_text(&doc, pos(0, 1, 0), pos(0, 4, 0)), "b\n1\t2");
    }

    #[test]
    fn select_all_skips_rows_without_text_and_positions_survive_reloads() {
        let old = doc("---\n\nab\n\n---\n");
        assert_eq!(select_all(&old), Some((pos(1, 0, 0), pos(1, 0, 2))));
        // 别处改了、这一段没变：位置还在。
        let new = doc("x\n\nab\n\nmore\n");
        assert!(still_valid(&old, &new, pos(1, 0, 2)));
        assert!(!still_valid(&old, &new, pos(1, 0, 3)), "超出这一段");
        assert!(!still_valid(&old, &new, pos(0, 0, 0)), "旧文档这里没有文字");
        // 前面插了一段，行号对不上了。
        assert!(!still_valid(&old, &doc("new\n\n---\n\nab\n"), pos(1, 0, 0)));
        let wide = doc("中");
        assert!(!still_valid(&wide, &wide, pos(0, 0, 1)), "不落在字符边界上");
    }
}
