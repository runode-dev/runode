//! 比较渲染循环中逐个 getter 读取单元格与 `CellIteration::read` 批量读取的
//! 耗时，并顺带核对两者结果一致。
//!
//! 默认忽略，手动运行：
//!
//! ```sh
//! cargo test --release -p libghostty-vt --test render_read_bench -- --ignored --nocapture
//! ```

use std::time::{Duration, Instant};

use libghostty_vt::{
    RenderState, Terminal,
    render::{CellIterator, RowIterator},
    screen::CellWide,
    style::RgbColor,
};

/// 一屏带颜色和样式的文本，模拟 `ls --color` 或编辑器界面。
fn colorful_terminal(cols: u16, rows: u16) -> Terminal<'static, 'static> {
    let mut terminal = Terminal::new(cols, rows).unwrap();
    let mut text = Vec::new();
    for y in 0..rows {
        for x in 0..cols / 8 {
            let color = 31 + (x + y) % 7;
            text.extend_from_slice(format!("\x1b[{color}mword{x:03}\x1b[0m ").as_bytes());
        }
        if y + 1 < rows {
            text.extend_from_slice(b"\r\n");
        }
    }
    terminal.vt_write(&text);
    terminal
}

/// 渲染循环关心的一个单元格的数据。
#[derive(Debug, PartialEq)]
struct Out {
    wide: bool,
    text: String,
    fg: Option<RgbColor>,
    bg: Option<RgbColor>,
    bold: bool,
}

fn frame_with_getters(
    state: &mut RenderState<'static>,
    terminal: &Terminal<'static, 'static>,
    rows: &mut RowIterator<'static>,
    cells: &mut CellIterator<'static>,
    out: &mut Vec<Out>,
) {
    out.clear();
    let snapshot = state.update(terminal).unwrap();
    let mut row_iter = rows.update(&snapshot).unwrap();
    while let Some(row) = row_iter.next() {
        let mut cell_iter = cells.update(row).unwrap();
        while let Some(cell) = cell_iter.next() {
            let wide = cell.raw_cell().unwrap().wide().unwrap() == CellWide::Wide;
            let mut text = String::new();
            if cell.graphemes_len().unwrap() > 0 {
                cell.graphemes_utf8(&mut text).unwrap();
            }
            let fg = cell.fg_color().unwrap();
            let bg = cell.bg_color().unwrap();
            let bold = cell.has_styling().unwrap() && cell.style().unwrap().bold;
            out.push(Out { wide, text, fg, bg, bold });
        }
    }
}

fn frame_with_read(
    state: &mut RenderState<'static>,
    terminal: &Terminal<'static, 'static>,
    rows: &mut RowIterator<'static>,
    cells: &mut CellIterator<'static>,
    out: &mut Vec<Out>,
) {
    out.clear();
    let snapshot = state.update(terminal).unwrap();
    let palette = snapshot.colors().unwrap().palette;
    let mut text = String::new();
    let mut row_iter = rows.update(&snapshot).unwrap();
    while let Some(row) = row_iter.next() {
        let mut cell_iter = cells.update(row).unwrap();
        while let Some(cell) = cell_iter.next() {
            let data = cell.read(&palette, &mut text).unwrap();
            out.push(Out {
                wide: data.wide == CellWide::Wide,
                text: text.clone(),
                fg: data.fg_color,
                bg: data.bg_color,
                bold: data.has_styling && data.style.bold,
            });
        }
    }
}

fn time(mut f: impl FnMut(), frames: u32) -> Duration {
    // 先热身一帧，避免首帧的分配影响结果。
    f();
    let start = Instant::now();
    for _ in 0..frames {
        f();
    }
    start.elapsed() / frames
}

#[test]
#[expect(clippy::print_stdout, reason = "量出的耗时要打出来看")]
#[ignore = "benchmark; run manually with --ignored --nocapture"]
fn render_read_vs_getters() {
    let (cols, rows) = (200, 60);
    let terminal = colorful_terminal(cols, rows);
    let mut state = RenderState::new().unwrap();
    let mut row_it = RowIterator::new().unwrap();
    let mut cell_it = CellIterator::new().unwrap();
    let mut a = Vec::new();
    let mut b = Vec::new();

    frame_with_getters(&mut state, &terminal, &mut row_it, &mut cell_it, &mut a);
    frame_with_read(&mut state, &terminal, &mut row_it, &mut cell_it, &mut b);
    assert_eq!(a, b, "batched results must match the individual getters");

    let frames = 200;
    let getters = time(|| frame_with_getters(&mut state, &terminal, &mut row_it, &mut cell_it, &mut a), frames);
    let read = time(|| frame_with_read(&mut state, &terminal, &mut row_it, &mut cell_it, &mut b), frames);
    let cells = u32::from(cols) * u32::from(rows);
    println!(
        "{cols}x{rows} ({cells} cells), {frames} frames: getters {getters:?}/frame \
         ({:?}/cell), read {read:?}/frame ({:?}/cell), speedup {:.2}x",
        getters / cells,
        read / cells,
        getters.as_secs_f64() / read.as_secs_f64(),
    );
}
