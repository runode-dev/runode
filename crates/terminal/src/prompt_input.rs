//! 从屏幕上读 shell 提示符后面的输入：正在编辑的那条命令，以及刚提交、开始运行的那条命令。
//!
//! 两者都靠 shell 集成用 OSC 133 打在单元格上的语义标记分清提示符和用户输入，没有标记时
//! 什么也读不到。这里只读终端状态、不改它，所以 VT 的回调里也能用。

use libghostty_vt::{
    Terminal,
    error::{Error, Result},
    screen::{CellSemanticContent, CellWide, GridRef, RowSemanticPrompt, Screen},
    terminal::{Point, PointCoordinate},
};

/// 光标所在的那条输入：提示符之后的文字，连同软换行接在一起的上下几行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptInput {
    /// 输入的全部文字，末尾的空白去掉了，光标前的空白保留。
    pub text: String,
    /// 光标在 `text` 里的字节位置。
    pub cursor: usize,
    /// 光标之后（这一行剩下的单元格，以及软换行接在后面的行）一个字也没有。
    pub at_end: bool,
}

impl PromptInput {
    /// 光标之前的那部分输入。
    pub fn before_cursor(&self) -> &str {
        &self.text[..self.cursor]
    }
}

/// 一个单元格里的内容。
enum Slot {
    /// 宽字符占的第二格，或者行尾放不下宽字符时空出来的那一格，不算字符。
    Spacer,
    Blank,
    Text(String),
}

impl Slot {
    /// 接到 `text` 后面：空白格算一个空格，占位格不算。
    fn push_to(&self, text: &mut String) {
        match self {
            Slot::Spacer => {}
            Slot::Blank => text.push(' '),
            Slot::Text(s) => text.push_str(s),
        }
    }
}

/// 光标正停在 shell 提示符上时，读出提示符后面的输入。备用屏幕上、光标不在提示符上，
/// 或者光标所在的这条输入里找不到 shell 集成标出的提示符时为 `None`。
///
/// 读的是活动区，不管视口有没有翻到回滚历史里。
pub fn read(terminal: &Terminal<'_, '_>) -> Result<Option<PromptInput>> {
    if terminal.active_screen()? == Screen::Alternate || !terminal.is_cursor_at_prompt()? {
        return Ok(None);
    }
    let cols = terminal.cols()?;
    let rows = u32::from(terminal.rows()?);
    let (cursor_x, cursor_y) = (terminal.cursor_x()?, u32::from(terminal.cursor_y()?));
    let row = |y: u32| terminal.grid_ref(Point::Active(PointCoordinate { x: 0, y }))?.row();
    let mut top = cursor_y;
    while top > 0 && row(top)?.is_wrap_continuation()? {
        top -= 1;
    }
    let mut bottom = cursor_y;
    while bottom + 1 < rows && row(bottom)?.is_wrapped()? {
        bottom += 1;
    }

    // 各行的单元格按顺序连成一串，各自记下是不是提示符。
    let mut slots = Vec::with_capacity((bottom - top + 1) as usize * usize::from(cols));
    for y in top..=bottom {
        for x in 0..cols {
            let grid_ref = terminal.grid_ref(Point::Active(PointCoordinate { x, y }))?;
            let prompt = grid_ref.cell()?.semantic_content()? == CellSemanticContent::Prompt;
            slots.push((slot(&grid_ref)?, prompt));
        }
    }
    let cursor = (cursor_y - top) as usize * usize::from(cols) + usize::from(cursor_x);
    // 输入从光标前最后一个提示符单元格之后开始。光标后面的提示符单元格是画在行尾的右侧
    // 提示符，不算输入，也不挡住「光标在输入末尾」。
    let Some(start) = slots[..cursor].iter().rposition(|(_, prompt)| *prompt).map(|i| i + 1) else {
        return Ok(None);
    };

    let mut text = String::new();
    for (slot, _) in &slots[start..cursor] {
        slot.push_to(&mut text);
    }
    let before = text.len();
    let rest: Vec<&Slot> = slots[cursor..].iter().filter(|(_, prompt)| !prompt).map(|(slot, _)| slot).collect();
    for slot in &rest {
        slot.push_to(&mut text);
    }
    text.truncate(before + text[before..].trim_end().len());
    Ok(Some(PromptInput { text, cursor: before, at_end: !rest.iter().any(|slot| matches!(slot, Slot::Text(_))) }))
}

/// 刚提交的那条命令：从光标所在行往上，把 shell 集成标为用户输入的单元格读出来，直到主
/// 提示符所在的行。在 shell 报告命令开始运行（OSC 133;C）的那一刻调用，这时命令还原样留在
/// 屏幕上，光标在它下面。
///
/// 软换行接起来，续行提示符分开的各行之间用换行连接，每行末尾的空白去掉。没读到输入，
/// 或者往上直到活动区顶上都没找到主提示符（命令太长、开头已经滚进回滚历史）时为 `None`，
/// 免得记下半条命令。
pub fn submitted_command(terminal: &Terminal<'_, '_>) -> Result<Option<String>> {
    let cols = terminal.cols()?;
    // 从下往上读到的各行：行里用户输入的文字，以及这一行是不是上一行软换行接下来的。
    let mut lines: Vec<(String, bool)> = Vec::new();
    let mut found_prompt = false;
    let mut y = u32::from(terminal.cursor_y()?);
    loop {
        let row = terminal.grid_ref(Point::Active(PointCoordinate { x: 0, y }))?.row()?;
        let mut text = String::new();
        let (mut input, mut prompt) = (false, false);
        for x in 0..cols {
            let grid_ref = terminal.grid_ref(Point::Active(PointCoordinate { x, y }))?;
            match grid_ref.cell()?.semantic_content()? {
                CellSemanticContent::Input => {
                    input = true;
                    slot(&grid_ref)?.push_to(&mut text);
                }
                CellSemanticContent::Prompt => prompt = true,
                CellSemanticContent::Output => {}
            }
        }
        // 光标所在行通常是命令下面新起的空行，开始读到输入之前的行跳过。
        if input || prompt || !lines.is_empty() {
            if !input && !prompt {
                break;
            }
            lines.push((text, row.is_wrap_continuation()?));
        }
        if prompt && row.semantic_prompt()? == RowSemanticPrompt::Prompt {
            found_prompt = true;
            break;
        }
        if y == 0 {
            break;
        }
        y -= 1;
    }
    if !found_prompt {
        return Ok(None);
    }
    let mut command = String::new();
    for (text, continuation) in lines.iter().rev() {
        if !command.is_empty() && !continuation {
            command.truncate(command.trim_end().len());
            command.push('\n');
        }
        command.push_str(text);
    }
    command.truncate(command.trim_end().len());
    Ok((!command.trim().is_empty()).then_some(command))
}

fn slot(grid_ref: &GridRef<'_>) -> Result<Slot> {
    let cell = grid_ref.cell()?;
    if matches!(cell.wide()?, CellWide::SpacerTail | CellWide::SpacerHead) {
        return Ok(Slot::Spacer);
    }
    if !cell.has_text()? {
        return Ok(Slot::Blank);
    }
    let mut buf = ['\0'; 16];
    let len = match grid_ref.graphemes(&mut buf) {
        Ok(len) => len,
        // 组合字符多得放不下时只取基本字符。
        Err(Error::OutOfSpace { .. }) => {
            return Ok(Slot::Text(char::from_u32(cell.codepoint()?).unwrap_or(' ').to_string()));
        }
        Err(err) => return Err(err),
    };
    Ok(Slot::Text(buf[..len].iter().collect()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn terminal(cols: u16, rows: u16) -> Terminal<'static, 'static> {
        Terminal::new(cols, rows).unwrap()
    }

    #[test]
    fn reads_the_input_after_a_marked_prompt() {
        let mut t = terminal(40, 5);
        t.vt_write(b"\x1b]133;A\x07~ % \x1b]133;B\x07git st");
        let input = read(&t).unwrap().unwrap();
        assert_eq!(input.text, "git st");
        assert_eq!(input.before_cursor(), "git st");
        assert!(input.at_end);
        // 光标往左挪两格：光标之后还有字。
        t.vt_write(b"\x1b[2D");
        let input = read(&t).unwrap().unwrap();
        assert_eq!(input.before_cursor(), "git ");
        assert!(!input.at_end);
    }

    #[test]
    fn keeps_blanks_before_the_cursor() {
        let mut t = terminal(40, 5);
        t.vt_write(b"\x1b]133;A\x07$ \x1b]133;B\x07git ");
        let input = read(&t).unwrap().unwrap();
        assert_eq!(input.before_cursor(), "git ");
        assert_eq!(input.text, "git ");
    }

    #[test]
    fn follows_soft_wraps() {
        let mut t = terminal(10, 5);
        t.vt_write(b"\x1b]133;A\x07$ \x1b]133;B\x07echo hello wor");
        let input = read(&t).unwrap().unwrap();
        assert_eq!(input.before_cursor(), "echo hello wor");
    }

    #[test]
    fn needs_a_marked_prompt_and_the_cursor_on_it() {
        let mut t = terminal(40, 5);
        t.vt_write(b"$ ls");
        assert_eq!(read(&t).unwrap(), None);
        t.vt_write(b"\r\n\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07");
        assert_eq!(read(&t).unwrap(), None);
    }

    #[test]
    fn text_after_the_cursor_on_the_row_means_not_at_the_end() {
        let mut t = terminal(40, 5);
        // 光标停在 `ls` 后面，右边还画着别的东西。
        t.vt_write(b"\x1b]133;A\x07$ \x1b]133;B\x07ls\x1b[30Gabc\x1b[5G");
        let input = read(&t).unwrap().unwrap();
        assert_eq!(input.before_cursor(), "ls");
        assert!(!input.at_end);
    }

    /// 提示符画完以后，像 zsh 那样在行尾画右侧提示符，再把光标挪回输入处；右侧提示符用
    /// shell 集成脚本发的 `133;P;k=r` 和 `133;B` 包着。
    const RIGHT_PROMPT: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07\x1b[35G\x1b]133;P;k=r\x0712:34\x1b]133;B\x07\x1b[3G";

    #[test]
    fn a_right_prompt_is_neither_input_nor_in_the_way() {
        let mut t = terminal(40, 5);
        t.vt_write(RIGHT_PROMPT);
        t.vt_write(b"git st");
        let input = read(&t).unwrap().unwrap();
        assert_eq!((input.text.as_str(), input.before_cursor(), input.at_end), ("git st", "git st", true));
        // 光标往左挪两格：光标后面还有输入，右侧提示符照样不算。
        t.vt_write(b"\x1b[2D");
        let input = read(&t).unwrap().unwrap();
        assert_eq!((input.text.as_str(), input.before_cursor(), input.at_end), ("git st", "git ", false));
    }

    #[test]
    fn submitted_command_leaves_out_the_right_prompt() {
        let mut t = terminal(40, 6);
        let mut bytes = RIGHT_PROMPT.to_vec();
        bytes.extend_from_slice(b"git status\r\n\x1b]133;C\x07");
        assert_eq!(submitted(&mut t, &bytes), [Some("git status".to_owned())]);
    }

    /// 在 OSC 133;C 的回调里读刚提交的命令。
    fn submitted(t: &mut Terminal<'static, 'static>, bytes: &[u8]) -> Vec<Option<String>> {
        let seen = std::rc::Rc::new(RefCell::new(Vec::new()));
        t.on_semantic_prompt({
            let seen = seen.clone();
            move |term, event| {
                if let libghostty_vt::terminal::SemanticPrompt::OutputStart { .. } = event {
                    seen.borrow_mut().push(submitted_command(term).unwrap());
                }
            }
        })
        .unwrap();
        t.vt_write(bytes);
        seen.take()
    }

    #[test]
    fn submitted_command_is_read_when_it_starts() {
        let mut t = terminal(40, 6);
        let seen = submitted(&mut t, b"\x1b]133;A\x07~ % \x1b]133;B\x07git status  \r\n\x1b]133;C\x07output\r\n");
        assert_eq!(seen, [Some("git status".to_owned())]);
    }

    #[test]
    fn submitted_command_joins_soft_wraps_and_continuation_lines() {
        let mut t = terminal(10, 8);
        let seen = submitted(
            &mut t,
            b"\x1b]133;A\x07$ \x1b]133;B\x07echo 12345678 \\\r\n\
              \x1b]133;A;k=s\x07> \x1b]133;B\x07done\r\n\x1b]133;C\x07",
        );
        assert_eq!(seen, [Some("echo 12345678 \\\ndone".to_owned())]);
    }

    #[test]
    fn submitted_command_with_a_two_line_prompt() {
        let mut t = terminal(40, 6);
        let seen = submitted(&mut t, b"\x1b]133;A\x07~/src\r\n> \x1b]133;B\x07make\r\n\x1b]133;C\x07");
        assert_eq!(seen, [Some("make".to_owned())]);
    }

    #[test]
    fn submitted_command_skips_an_empty_line_and_a_lost_prompt() {
        let mut t = terminal(40, 6);
        let seen = submitted(&mut t, b"\x1b]133;A\x07$ \x1b]133;B\x07\r\n\x1b]133;C\x07");
        assert_eq!(seen, [None]);
        // 提示符已经滚出活动区：只剩半条命令，不要。
        let mut t = terminal(10, 2);
        let seen = submitted(&mut t, b"\x1b]133;A\x07$ \x1b]133;B\x07aaaaaaaabbbbbbbbbbcccccccccc\r\n\x1b]133;C\x07");
        assert_eq!(seen, [None]);
    }
}
