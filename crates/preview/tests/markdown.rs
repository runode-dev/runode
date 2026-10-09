//! Markdown 解析的公开接口：每种块、嵌套列表、任务列表、表格对齐、图片断开段落、HTML 原样留着，
//! 以及照 GitHub 的提示块、脚注、裸网址链接、标题锚点和 `<kbd>`。

use std::{path::Path, sync::atomic::AtomicBool};

use runode_preview::{
    Alert, Align, Block, Color, Footnote, Inline, InlineStyle, ListItem, is_markdown, parse_markdown,
};

fn parse(text: &str) -> Vec<Block> {
    parse_markdown(text, &AtomicBool::new(false)).unwrap()
}

fn plain(text: &str) -> Inline {
    Inline { text: text.to_owned(), ..Inline::default() }
}

fn styled(text: &str, style: InlineStyle) -> Inline {
    Inline { text: text.to_owned(), style, link: None }
}

fn para(text: &str) -> Block {
    Block::Paragraph(vec![plain(text)])
}

fn item(blocks: Vec<Block>) -> ListItem {
    ListItem { task: None, blocks }
}

#[test]
fn recognizes_markdown_extensions() {
    assert!(is_markdown(Path::new("README.md")));
    assert!(is_markdown(Path::new("a/notes.MARKDOWN")));
    assert!(is_markdown(Path::new("page.mdx")));
    assert!(!is_markdown(Path::new("main.rs")));
    assert!(!is_markdown(Path::new("md")));
}

#[test]
fn headings_keep_their_level() {
    assert_eq!(
        parse("# One\n\n### Three *x*\n"),
        vec![
            Block::Heading { level: 1, inlines: vec![plain("One")], id: "one".into() },
            Block::Heading {
                level: 3,
                inlines: vec![plain("Three "), styled("x", InlineStyle { italic: true, ..InlineStyle::default() })],
                id: "three-x".into(),
            },
        ]
    );
}

#[test]
fn inline_styles_and_links() {
    let blocks = parse("a **b** *c* ~~d~~ `e` [f **g**](https://x.dev)\nnext");
    let bold = InlineStyle { bold: true, ..InlineStyle::default() };
    assert_eq!(
        blocks,
        vec![Block::Paragraph(vec![
            plain("a "),
            styled("b", bold),
            plain(" "),
            styled("c", InlineStyle { italic: true, ..InlineStyle::default() }),
            plain(" "),
            styled("d", InlineStyle { strike: true, ..InlineStyle::default() }),
            plain(" "),
            styled("e", InlineStyle { code: true, ..InlineStyle::default() }),
            plain(" "),
            Inline { text: "f ".into(), style: InlineStyle::default(), link: Some("https://x.dev".into()) },
            Inline { text: "g".into(), style: bold, link: Some("https://x.dev".into()) },
            // 软换行当空格，和后面的文字并成一段。
            plain(" next"),
        ])]
    );
}

#[test]
fn hard_breaks_keep_the_line_break() {
    assert_eq!(parse("a  \nb"), vec![para("a\nb")]);
}

#[test]
fn nested_lists_and_ordered_start() {
    let blocks = parse("- a\n  - b\n    - c\n- d\n\n3. x\n4. y\n");
    assert_eq!(
        blocks,
        vec![
            Block::List {
                start: None,
                loose: false,
                items: vec![
                    item(vec![
                        para("a"),
                        Block::List {
                            start: None,
                            loose: false,
                            items: vec![item(vec![
                                para("b"),
                                Block::List { start: None, loose: false, items: vec![item(vec![para("c")])] },
                            ])],
                        },
                    ]),
                    item(vec![para("d")]),
                ],
            },
            Block::List { start: Some(3), loose: false, items: vec![item(vec![para("x")]), item(vec![para("y")])] },
        ]
    );
}

#[test]
fn loose_list_items_hold_several_blocks() {
    let blocks = parse("1. one\n\n   more\n\n2. two\n");
    assert_eq!(
        blocks,
        vec![Block::List {
            start: Some(1),
            loose: true,
            items: vec![item(vec![para("one"), para("more")]), item(vec![para("two")])],
        }]
    );
}

#[test]
fn task_lists() {
    let blocks = parse("- [ ] todo\n- [x] done\n- plain\n");
    assert_eq!(
        blocks,
        vec![Block::List {
            start: None,
            loose: false,
            items: vec![
                ListItem { task: Some(false), blocks: vec![para("todo")] },
                ListItem { task: Some(true), blocks: vec![para("done")] },
                item(vec![para("plain")]),
            ],
        }]
    );
}

#[test]
fn quotes_nest_and_hold_blocks() {
    let blocks = parse("> a\n>\n> > b\n>\n> - c\n");
    assert_eq!(
        blocks,
        vec![Block::Quote {
            alert: None,
            blocks: vec![
                para("a"),
                Block::Quote { alert: None, blocks: vec![para("b")] },
                Block::List { start: None, loose: false, items: vec![item(vec![para("c")])] },
            ],
        }]
    );
}

#[test]
fn code_blocks_keep_lines_and_get_highlighted() {
    let blocks = parse("```rust title\n// hi\nlet x = 1;\n```\n\n    indented\n");
    let [Block::Code { lang, lines, highlights }, Block::Code { lang: none, lines: plain_lines, highlights: empty }] =
        blocks.as_slice()
    else {
        panic!("{blocks:?}");
    };
    assert_eq!(lang.as_deref(), Some("rust"));
    assert_eq!(lines, &["// hi", "let x = 1;"]);
    assert_eq!(highlights.len(), 2);
    assert!(highlights[0].iter().any(|span| span.style.color == Color::Ansi(8)));
    assert_eq!(*none, None);
    assert_eq!(plain_lines, &["indented"]);
    assert!(empty.is_empty());
}

#[test]
fn unknown_code_languages_are_not_highlighted() {
    let blocks = parse("```no-such-lang\nx\n```\n");
    assert!(matches!(&blocks[..], [Block::Code { highlights, .. }] if highlights.is_empty()));
}

#[test]
fn tables_keep_alignment_and_inline_cells() {
    let blocks = parse("| a | b | c | d |\n|:--|:-:|--:|---|\n| **x** | y | | z |\n| 1 | 2 | 3 | 4 |\n");
    let bold = InlineStyle { bold: true, ..InlineStyle::default() };
    assert_eq!(
        blocks,
        vec![Block::Table {
            aligns: vec![Align::Left, Align::Center, Align::Right, Align::Left],
            head: vec![vec![plain("a")], vec![plain("b")], vec![plain("c")], vec![plain("d")]],
            rows: vec![
                vec![vec![styled("x", bold)], vec![plain("y")], vec![], vec![plain("z")]],
                vec![vec![plain("1")], vec![plain("2")], vec![plain("3")], vec![plain("4")]],
            ],
        }]
    );
}

#[test]
fn rules() {
    assert_eq!(parse("a\n\n---\n\nb"), vec![para("a"), Block::Rule, para("b")]);
}

#[test]
fn images_split_paragraphs() {
    let blocks = parse("before ![logo](img/a.png) after\n\n![one](a.png)\n![two](https://x.dev/b.svg)\n");
    assert_eq!(
        blocks,
        vec![
            para("before "),
            Block::Image { url: "img/a.png".into(), alt: "logo".into() },
            para(" after"),
            Block::Image { url: "a.png".into(), alt: "one".into() },
            Block::Image { url: "https://x.dev/b.svg".into(), alt: "two".into() },
        ]
    );
}

#[test]
fn images_in_headings_and_cells_leave_their_alt_text() {
    assert_eq!(
        parse("# ![icon](i.png) Title"),
        vec![Block::Heading { level: 1, inlines: vec![plain("icon Title")], id: "icon-title".into() }]
    );
    let blocks = parse("| a |\n|---|\n| ![cell](c.png) |\n");
    assert!(matches!(&blocks[..], [Block::Table { rows, .. }] if rows == &[vec![vec![plain("cell")]]]));
}

#[test]
fn html_is_kept_as_text() {
    let blocks = parse("<div align=\"center\">\n  <b>hi</b>\n</div>\n\na <b>K</b> b\n");
    assert_eq!(blocks, vec![para("<div align=\"center\">\n  <b>hi</b>\n</div>"), para("a <b>K</b> b")]);
}

#[test]
fn front_matter_is_a_yaml_code_block() {
    let blocks = parse("---\ntitle: x\n---\n\n# Doc\n");
    assert!(
        matches!(&blocks[0], Block::Code { lang: Some(lang), lines, .. } if lang == "yaml" && lines == &["title: x"])
    );
    assert!(matches!(&blocks[1], Block::Heading { level: 1, .. }));
}

#[test]
fn cancelled_parse_gives_nothing() {
    assert_eq!(parse_markdown("# a", &AtomicBool::new(true)), None);
}

#[test]
fn kbd_tags_style_their_text() {
    let kbd = InlineStyle { kbd: true, ..InlineStyle::default() };
    assert_eq!(
        parse("press <kbd>Cmd</kbd>+<KBD>C</KBD>"),
        vec![Block::Paragraph(vec![plain("press "), styled("Cmd", kbd), plain("+"), styled("C", kbd)])]
    );
}

#[test]
fn github_alerts_keep_their_kind() {
    let kinds = [
        ("NOTE", Alert::Note),
        ("TIP", Alert::Tip),
        ("IMPORTANT", Alert::Important),
        ("WARNING", Alert::Warning),
        ("CAUTION", Alert::Caution),
    ];
    for (tag, alert) in kinds {
        let blocks = parse(&format!("> [!{tag}]\n> body\n"));
        assert_eq!(blocks, vec![Block::Quote { alert: Some(alert), blocks: vec![para("body")] }], "{tag}");
    }
    // 不认识的标记是普通引用。
    assert!(matches!(&parse("> [!OTHER]\n> x\n")[..], [Block::Quote { alert: None, .. }]));
}

#[test]
fn footnotes_are_numbered_by_first_reference_and_collected_at_the_end() {
    let blocks = parse("a[^b] c[^a] d[^b]\n\n[^a]: first\n[^b]: second\n\n[^unused]: x\n\ntail\n");
    let note = |text: &str, label: &str| Inline {
        text: text.into(),
        style: InlineStyle { footnote: true, ..InlineStyle::default() },
        link: Some(format!("#fn-{label}")),
    };
    let back = |label: &str| Inline { text: " ↩".into(), link: Some(format!("#fnref-{label}")), ..Inline::default() };
    assert_eq!(
        blocks,
        vec![
            Block::Paragraph(vec![
                plain("a"),
                note("¹", "b"),
                plain(" c"),
                note("²", "a"),
                plain(" d"),
                note("¹", "b")
            ]),
            para("tail"),
            Block::Footnotes(vec![
                Footnote { label: "b".into(), blocks: vec![Block::Paragraph(vec![plain("second"), back("b")])] },
                Footnote { label: "a".into(), blocks: vec![Block::Paragraph(vec![plain("first"), back("a")])] },
            ]),
        ]
    );
}

#[test]
fn bare_urls_become_links_outside_code() {
    let link = |text: &str, url: &str| Inline { text: text.into(), link: Some(url.into()), ..Inline::default() };
    assert_eq!(
        parse("see https://x.dev/a_(b)). and www.y.io, `https://no.dev` [z](https://z.dev) xhttps://q.dev"),
        vec![Block::Paragraph(vec![
            plain("see "),
            link("https://x.dev/a_(b)", "https://x.dev/a_(b)"),
            plain("). and "),
            link("www.y.io", "http://www.y.io"),
            plain(", "),
            styled("https://no.dev", InlineStyle { code: true, ..InlineStyle::default() }),
            plain(" "),
            link("z", "https://z.dev"),
            plain(" xhttps://q.dev"),
        ])]
    );
    // 没有点的主机名、只有协议名的不算；代码块里的不变。
    assert_eq!(parse("http://localhost:3000 https://"), vec![para("http://localhost:3000 https://")]);
    assert!(matches!(&parse("```\nhttps://x.dev\n```\n")[..], [Block::Code { .. }]));
    // 表格单元格里的也变。
    let blocks = parse("| a |\n|---|\n| www.x.dev |\n");
    assert!(
        matches!(&blocks[..], [Block::Table { rows, .. }] if rows[0][0] == vec![link("www.x.dev", "http://www.x.dev")])
    );
}

#[test]
fn heading_ids_follow_github_slugs() {
    let ids: Vec<String> = parse("# Hello, World!\n## Hello World\n### hello world\n# 中文 标题\n# `code` & more\n")
        .into_iter()
        .filter_map(|block| match block {
            Block::Heading { id, .. } => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(ids, ["hello-world", "hello-world-1", "hello-world-2", "中文-标题", "code--more"]);
}

/// 网址后面紧跟中文标点时到标点为止；中文域名、路径里的汉字照算。
#[test]
fn bare_urls_stop_at_cjk_punctuation() {
    let link = |text: &str| Inline { text: text.into(), link: Some(text.into()), ..Inline::default() };
    assert_eq!(
        parse("见 https://example.com，然后"),
        vec![Block::Paragraph(vec![plain("见 "), link("https://example.com"), plain("，然后")])]
    );
    assert_eq!(parse("https://a.com/x。"), vec![Block::Paragraph(vec![link("https://a.com/x"), plain("。")])]);
    assert_eq!(parse("https://例子.测试/路径"), vec![Block::Paragraph(vec![link("https://例子.测试/路径")])]);
}

/// 一长串没有空白的 `(www.`：每个候选都是网址的开头，不能每个都扫到串尾。
#[test]
fn long_runs_of_autolink_candidates_parse_quickly() {
    let text = "(www.".repeat(100_000);
    let started = std::time::Instant::now();
    let blocks = parse(&text);
    assert!(started.elapsed() < std::time::Duration::from_secs(1), "took {:?}", started.elapsed());
    assert!(matches!(&blocks[..], [Block::Paragraph(_)]));
    // 网址最长认 2048 字节左右，再长的后面照留成文字。
    let long = format!("https://x.dev/{}", "a".repeat(5000));
    let Block::Paragraph(inlines) = &parse(&long)[0] else { panic!() };
    assert!(inlines[0].link.is_some() && inlines[0].text.len() <= 2052 && inlines[1].link.is_none());
}

/// 块树里引用、列表最深套几层。
fn depth(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .map(|block| match block {
            Block::Quote { blocks, .. } => 1 + depth(blocks),
            Block::List { items, .. } => 1 + items.iter().map(|item| depth(&item.blocks)).max().unwrap_or(0),
            Block::Footnotes(notes) => 1 + notes.iter().map(|note| depth(&note.blocks)).max().unwrap_or(0),
            _ => 0,
        })
        .max()
        .unwrap_or(0)
}

/// 十万层的引用、列表在默认栈大小的线程里解析、释放都不把栈撑爆；超出的层并进最里面那层，文字还在。
#[test]
fn deep_nesting_is_capped() {
    for (prefix, name) in [("> ", "quote"), ("- ", "list")] {
        let text = format!("{}deep\n", prefix.repeat(100_000));
        let (depth, has_text) = std::thread::spawn(move || {
            let blocks = parse(&text);
            let found = format!("{blocks:?}").contains("deep");
            (depth(&blocks), found)
        })
        .join()
        .unwrap();
        assert!((1..=65).contains(&depth), "{name}: {depth}");
        assert!(has_text, "{name}");
    }
    // 套得深但没到上限的照常嵌套。
    assert_eq!(depth(&parse(&format!("{}x\n", "> ".repeat(10)))), 10);
}
