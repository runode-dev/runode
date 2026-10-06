//! 提示符上输入的语法高亮：读屏幕上的输入，按 `runode_prompt_highlight` 分好类、按调色板
//! 定好颜色，绘制时换掉这些单元格的前景色、粗细和下划线。不写进屏幕，shell 照常回显。

use std::rc::Rc;

use runode_prompt_highlight::Shell;
use runode_shared_types::color::Rgb;
use runode_terminal::{PromptInput, session::Session};

use super::TerminalView;

/// 一格要换成的样子。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CellStyle {
    pub(super) fg: Rgb,
    pub(super) bold: bool,
    pub(super) underline: bool,
}

/// 算好的高亮：活动区里各单元格要换成的样子，按（行，列）排好。
#[derive(Debug, Default)]
pub(super) struct Highlight {
    cells: Vec<((u16, u16), CellStyle)>,
}

impl Highlight {
    pub(super) fn get(&self, x: u16, y: u16) -> Option<CellStyle> {
        let i = self.cells.binary_search_by_key(&(y, x), |(at, _)| *at).ok()?;
        Some(self.cells[i].1)
    }
}

impl TerminalView {
    /// 按读到的输入重算高亮；没在提示符上时清掉。
    pub(super) fn refresh_highlight(&mut self, input: Option<&PromptInput>) {
        self.highlight = Rc::default();
        if !self.config.command_highlighting {
            return;
        }
        let Some(input) = input else {
            return;
        };
        let blank = |at: usize| input.text[at..].starts_with(' ');
        // shell 自己画的灰字建议（zsh-autosuggestions 这类）接在光标后面、带着颜色，不算输入。
        let mut cells = input.cells.as_slice();
        while let Some((last, rest)) = cells.split_last()
            && last.at >= input.cursor
            && (last.styled || blank(last.at))
        {
            cells = rest;
        }
        // 剩下的字也带着颜色，说明 shell 自己在上色（装了高亮插件），不去盖它。
        if cells.iter().any(|cell| cell.styled && !blank(cell.at)) {
            return;
        }
        let end =
            cells.last().map_or(0, |cell| cell.at + input.text[cell.at..].chars().next().map_or(0, char::len_utf8));
        let text = &input.text[..end];
        let Some(session) = self.screen.live() else {
            return;
        };
        let shell = Shell { path: session.shell_path(), names: session.shell_names(), ..Default::default() };
        let spans = runode_prompt_highlight::highlight(text, &shell, session.prompt_cwd().as_deref());
        if spans.is_empty() {
            return;
        }
        // 各段按顺序叠上去，后面的盖过前面的。
        let mut kinds = vec![None; text.len()];
        for span in spans {
            kinds[span.range].fill(Some(span.kind));
        }
        let palette = session.palette();
        let mut styled: Vec<_> = cells
            .iter()
            .filter_map(|cell| {
                let style = kinds[cell.at]?.style();
                let fg = palette[usize::from(style.color)];
                Some(((cell.y, cell.x), CellStyle { fg, bold: style.bold, underline: style.underline }))
            })
            .collect();
        styled.sort_by_key(|(at, _)| *at);
        self.highlight = Rc::new(Highlight { cells: styled });
    }

    /// 现在该画出来的高亮：视口翻到回滚历史里时屏幕上的不是活动区，不画。
    pub(super) fn visible_highlight(&self) -> Option<Rc<Highlight>> {
        (self.config.command_highlighting && self.screen.live().is_some_and(Session::viewport_at_bottom))
            .then(|| self.highlight.clone())
    }
}
