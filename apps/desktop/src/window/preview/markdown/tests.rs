use std::sync::atomic::AtomicBool;

use super::*;

fn doc(text: &str) -> Doc {
    Doc::new(runode_preview::parse_markdown(text, &AtomicBool::new(false)).unwrap(), Path::new("/repo"), |_| None)
}

/// 各行的套层，只看引用的提示种类和列表项的记号。
fn nests(doc: &Doc) -> Vec<Vec<String>> {
    doc.rows
        .iter()
        .map(|row| {
            row.nest
                .iter()
                .map(|level| match level {
                    Nest::Quote { alert, .. } => format!("quote {alert:?}"),
                    Nest::Item { marker, .. } => format!("item {marker:?}"),
                })
                .collect()
        })
        .collect()
}

#[test]
fn nested_lists_flatten_with_markers_on_first_rows() {
    let doc = doc("- a\n\n  more\n  - b\n- c\n\n1. x\n");
    assert_eq!(
        nests(&doc),
        vec![
            vec!["item Some(Bullet)"],
            vec!["item None"],
            vec!["item None", "item Some(Bullet)"],
            vec!["item Some(Bullet)"],
            vec!["item Some(Number(1))"],
        ]
    );
    // 松散列表里的段落之间、相邻的项之间都空 1em；挨着的另一个列表不算同一个列表。
    assert_eq!(gap(&doc.rows, 0), 0.);
    assert_eq!(gap(&doc.rows, 1), 1.);
    assert_eq!(gap(&doc.rows, 3), 1.);
    assert_eq!(gap(&doc.rows, 4), 1.);
}

#[test]
fn tight_lists_keep_items_close_and_sublists_flush() {
    let doc = doc("intro\n- a\n  - b\n- c\n\n# Title\n\n---\n\nend\n");
    // 段落到列表 1em，子列表紧贴上一级，相邻项 0.25em，标题上面 1.5em，分割线上下 1.5em。
    assert_eq!((1..doc.rows.len()).map(|ix| gap(&doc.rows, ix)).collect::<Vec<_>>(), [1., 0., 0.25, 1.5, 1.5, 1.5]);
}

#[test]
fn list_inside_item_shows_both_markers_on_one_row() {
    let doc = doc("- - [x] deep\n- \n");
    assert_eq!(nests(&doc), vec![vec!["item Some(Bullet)", "item Some(Task(true))"], vec!["item Some(Bullet)"]]);
    // 空的列表项也有一行。
    assert!(matches!(&doc.rows[1].leaf, Leaf::Paragraph(rich) if rich.text.is_empty()));
}

#[test]
fn quotes_keep_their_bar_across_rows_and_alerts_get_a_title() {
    let doc = doc("intro\n\n> a\n>\n> - b\n\n> [!WARNING]\n> careful\n\nafter\n");
    assert_eq!(
        nests(&doc),
        vec![
            vec![],
            vec!["quote None"],
            vec!["quote None", "item Some(Bullet)"],
            vec!["quote Some(Warning)"],
            vec!["quote Some(Warning)"],
            vec![],
        ]
    );
    assert!(!continues(doc.rows.first(), &doc.rows[1], 0), "引用的第一行上面留空");
    assert!(continues(doc.rows.get(1), &doc.rows[2], 0), "引用里的后几行竖线接着画");
    assert!(!continues(doc.rows.get(2), &doc.rows[3], 0), "挨着的另一段引用另画竖线");
    assert!(matches!(&doc.rows[3].leaf, Leaf::AlertTitle(Alert::Warning, title) if !title.is_empty()));
    assert_eq!(gap(&doc.rows, 4), 0.5);
}

#[test]
fn ordered_markers_change_style_by_depth() {
    assert_eq!(marker_text(Marker::Number(3), 1), "3.");
    assert_eq!(marker_text(Marker::Number(4), 2), "iv.");
    assert_eq!(marker_text(Marker::Number(1994), 2), "mcmxciv.");
    assert_eq!(marker_text(Marker::Number(2), 3), "b.");
    assert_eq!(marker_text(Marker::Number(28), 4), "ab.");
    assert_eq!(marker_text(Marker::Task(false), 1), "[ ]");
}

#[test]
fn headings_and_footnotes_become_anchors() {
    let doc = doc("# Intro\n\ntext[^n]\n\n## Intro\n\n[^n]: the note\n");
    assert_eq!(doc.anchor("intro"), Some(0));
    assert_eq!(doc.anchor("intro-1"), Some(2));
    assert_eq!(doc.anchor("Intro"), Some(0), "不分大小写也能对上");
    assert_eq!(doc.anchor("fnref-n"), Some(1));
    // 脚注前面是一条细线，定义在它下面，带编号。
    assert!(matches!(doc.rows[3].leaf, Leaf::Rule) && doc.rows[3].footnote);
    assert_eq!(doc.anchor("fn-n"), Some(4));
    assert_eq!(nests(&doc)[4], ["item Some(Number(1))"]);
    let Leaf::Paragraph(rich) = &doc.rows[4].leaf else { panic!() };
    assert_eq!(rich.text.as_ref(), "the note ↩");
    assert_eq!(doc.link_at((4, 0), 9), Some((0, "#fnref-n")));
}

#[test]
fn rich_text_records_styles_and_merges_links() {
    let doc = doc("a **b** [c `d`](x.md) e");
    let Leaf::Paragraph(rich) = &doc.rows[0].leaf else { panic!() };
    assert_eq!(rich.text.as_ref(), "a b c d e");
    let bold = InlineStyle { bold: true, ..InlineStyle::default() };
    let code = InlineStyle { code: true, ..InlineStyle::default() };
    assert_eq!(rich.runs, vec![(2..3, bold, false), (4..6, InlineStyle::default(), true), (6..7, code, true)]);
    assert_eq!(rich.links, vec![(4..7, "x.md".to_owned())]);
    let colors = Colors::new(Rgb(0xEE, 0xEE, 0xEE), Rgb(0x11, 0x11, 0x11), &runode_shared_types::theme::ANSI);
    let highlights = rich_highlights(rich, colors, None);
    assert_eq!(highlights[0].1.font_weight, Some(FontWeight::SEMIBOLD));
    assert_eq!(highlights[1].1.color, Some(colors.link));
    assert!(highlights[1].1.underline.is_none(), "链接平时没有下划线");
    // 鼠标在链接上时整个链接带下划线。
    let hovered = rich_highlights(rich, colors, Some(0));
    assert!(hovered[1].1.underline.is_some() && hovered[2].1.underline.is_some());
    assert_eq!(doc.link_at((0, 0), 5), Some((0, "x.md")));
    assert_eq!(doc.link_at((0, 0), 1), None);
}

#[test]
fn code_blocks_expand_tabs_and_shift_spans() {
    let doc = doc("```rust\nfn a() {}\n\t// x\n```\n");
    let Leaf::Code { text, spans, source } = &doc.rows[0].leaf else { panic!() };
    assert_eq!(text.as_ref(), "fn a() {}\n    // x");
    assert_eq!(source.as_ref(), "fn a() {}\n\t// x");
    // 第二行的注释在展开后的位置上。
    assert!(spans.iter().any(|span| span.range == (14..18)));
}

#[test]
fn tables_and_images_become_rows() {
    let doc = Doc::new(
        runode_preview::parse_markdown(
            "| a | b |\n|---|--:|\n| 1 | 2 |\n\n![logo](a.png)\n![](https://x/y.png)\n",
            &AtomicBool::new(false),
        )
        .unwrap(),
        Path::new("/repo"),
        |path| (path == Path::new("/repo/a.png")).then(|| Picture::Svg(Arc::new(RenderImage::new(Vec::new())))),
    );
    let Leaf::Table { aligns, head, rows } = &doc.rows[0].leaf else { panic!() };
    assert_eq!(aligns, &[Align::Left, Align::Right]);
    assert_eq!(head[1].text.as_ref(), "b");
    assert_eq!(rows[0][0].text.as_ref(), "1");
    assert_eq!(doc.rows[0].leaf.texts(), ["a", "b", "1", "2"]);
    assert!(matches!(&doc.rows[1].leaf, Leaf::Image { alt, picture: Some(_), remote: false, .. } if alt == "logo"));
    // 网络图片不经本地读图，等后台下载；没有替代文字时显示地址。
    assert!(
        matches!(&doc.rows[2].leaf, Leaf::Image { alt, picture: None, remote: true, .. } if alt == "https://x/y.png")
    );
    assert_eq!(doc.remote_urls(), ["https://x/y.png"]);
}

#[test]
fn links_resolve_against_the_file() {
    let dir = Path::new("/repo/docs");
    assert_eq!(link_target(dir, "https://x.dev/a"), Some(LinkTarget::Web("https://x.dev/a".into())));
    assert_eq!(link_target(dir, "HTTP://x.dev"), Some(LinkTarget::Web("HTTP://x.dev".into())));
    assert_eq!(link_target(dir, "mailto:a@b.c"), Some(LinkTarget::Web("mailto:a@b.c".into())));
    // `..` 和 `.` 按字面消掉，和文件树打开的路径一样。
    assert_eq!(link_target(dir, "../README.md#usage"), Some(LinkTarget::File("/repo/README.md".into())));
    assert_eq!(link_target(dir, "./a/../b.md"), Some(LinkTarget::File("/repo/docs/b.md".into())));
    assert_eq!(link_target(dir, "../../../../x.md"), Some(LinkTarget::File("/x.md".into())));
    assert_eq!(link_target(dir, "img/a.png?raw=1"), Some(LinkTarget::File("/repo/docs/img/a.png".into())));
    assert_eq!(link_target(dir, "/etc/hosts"), Some(LinkTarget::File("/etc/hosts".into())));
    assert_eq!(link_target(dir, "#usage"), Some(LinkTarget::Anchor("usage".into())));
    assert_eq!(link_target(dir, "#"), None);
    assert_eq!(link_target(dir, "javascript:alert(1)"), None);
    assert_eq!(link_target(dir, "vscode://file/a"), None);
}

#[test]
fn local_links_and_anchors_are_percent_decoded() {
    let dir = Path::new("/repo");
    assert_eq!(link_target(dir, "my%20file.md"), Some(LinkTarget::File("/repo/my file.md".into())));
    assert_eq!(link_target(dir, "#%E4%B8%AD%E6%96%87"), Some(LinkTarget::Anchor("中文".into())));
    // 不成对的 `%`、解出来不是 UTF-8 的原样留着。
    assert_eq!(percent_decode("100%"), "100%");
    assert_eq!(percent_decode("a%zzb"), "a%zzb");
    assert_eq!(percent_decode("%FF"), "%FF");
}

fn bitmap(byte: u8) -> Picture {
    Picture::Bitmap(Arc::new(Image::from_bytes(gpui::ImageFormat::Png, vec![byte])))
}

/// 同一个本地图片文件不管写成什么样的地址只读一次，不同的文件最多读 `MAX_LOCAL_IMAGES` 个。
#[test]
fn local_images_are_read_once_per_file_and_capped() {
    let reads = Cell::new(0);
    let read = |_: &Path| {
        reads.set(reads.get() + 1);
        Some(bitmap(1))
    };
    let parse = |text: &str| runode_preview::parse_markdown(text, &AtomicBool::new(false)).unwrap();
    let doc = Doc::new(parse("![a](x.png)\n\n![b](./x.png)\n\n![c](sub/../x.png)\n"), Path::new("/repo"), read);
    assert_eq!(reads.get(), 1);
    assert_eq!(doc.pictures().count(), 3);

    reads.set(0);
    let many: String = (0..MAX_LOCAL_IMAGES + 10).map(|ix| format!("![{ix}](img{ix}.png)\n\n")).collect();
    let doc = Doc::new(parse(&many), Path::new("/repo"), read);
    assert_eq!(reads.get(), MAX_LOCAL_IMAGES);
    let rows: Vec<bool> = doc.rows.iter().map(|row| matches!(row.leaf, Leaf::Image { picture: Some(_), .. })).collect();
    assert!(rows[..MAX_LOCAL_IMAGES].iter().all(|&shown| shown));
    assert!(rows[MAX_LOCAL_IMAGES..].iter().all(|&shown| !shown), "超出的只显示替代文字");
}

#[test]
fn remote_images_are_deduplicated_and_capped() {
    let mut text = String::from("![](https://x/a.png)\n\n![](https://x/a.png)\n\n");
    text.extend((0..MAX_REMOTE_IMAGES + 10).map(|ix| format!("![](https://x/{ix}.png)\n\n")));
    let urls = doc(&text).remote_urls();
    assert_eq!(urls.len(), MAX_REMOTE_IMAGES);
    assert_eq!(urls[0], "https://x/a.png");
    assert_eq!(urls[1], "https://x/0.png");
}

/// 换上新文档时只放掉新文档不再用到的图：同样字节的位图留着。
#[test]
fn reloads_release_only_pictures_the_new_doc_drops() {
    let parse = |text: &str| runode_preview::parse_markdown(text, &AtomicBool::new(false)).unwrap();
    let pictures = |path: &Path| match path.file_name()?.to_str()? {
        "keep.png" => Some(bitmap(1)),
        "gone.png" => Some(bitmap(2)),
        _ => None,
    };
    let old = Doc::new(parse("![](keep.png)\n\n![](gone.png)\n"), Path::new("/repo"), pictures);
    let new = Doc::new(parse("text\n\n![](keep.png)\n"), Path::new("/repo"), pictures);
    let unused: Vec<_> = old.unused_pictures(&new).into_iter().map(Picture::key).collect();
    assert!(unused == [bitmap(2).key()], "只放掉 gone.png");
}
