//! 指针下能打开的东西：程序用 OSC 8 标出的超链接，或者屏幕文字里的网址和文件路径。界面在 ⌘ 点击时
//! 打开它，按着 ⌘ 悬停时给它画下划线。

use std::{
    ops::Range,
    path::{Path, PathBuf},
};

use libghostty_vt::{
    Terminal,
    error::{Error, Result},
    screen::{CellWide, GridRef, Row},
    terminal::{Point, PointCoordinate},
};
use runode_shared_types::grid::GridPoint;

use super::{Session, log_err};

/// 能打开的东西。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    /// 网址，或者程序用 OSC 8 标出的 URI，交给系统按协议打开。
    Url(String),
    /// 存在的文件或目录。
    Path(PathBuf),
}

/// 指针下的链接，以及它在视口里占的单元格。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub target: LinkTarget,
    /// 每行一段，从上往下：软换行折开的链接占几行就有几段。
    pub spans: Vec<LinkSpan>,
}

/// 链接在视口第 `y` 行占的列。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkSpan {
    pub y: u16,
    pub x: Range<u16>,
}

impl Link {
    /// 视口里 (`x`, `y`) 这一格在不在链接上。
    pub fn contains(&self, x: u16, y: u16) -> bool {
        self.spans.iter().any(|span| span.y == y && span.x.contains(&x))
    }
}

/// 逻辑行（软换行接起来的几行）里的一格。宽字符只记前一格，宽度为 2。
#[derive(Clone)]
struct LineCell {
    x: u16,
    y: u16,
    width: u16,
    /// 空白格是一个空格。
    text: String,
    uri: Option<String>,
}

impl Session {
    /// 视口里 `at` 这一格上的链接：程序用 OSC 8 标了超链接的就是它；没标的在这一行（连同软换行接着的
    /// 上下几行，以及 `join_hard_wraps` 认出的硬换行续行）的文字里找指针所在的网址或路径。路径只认存在的，
    /// 相对路径按 shell 当前所在的目录解析，不知道目录时只认绝对路径和 `~/` 开头的。
    pub fn link_at(&self, at: GridPoint) -> Option<Link> {
        let size = self.size.get();
        if at.x < 0. || at.y < 0. || at.x >= f32::from(size.cols) || at.y >= f32::from(size.rows) {
            return None;
        }
        let (x, y) = (at.x as u16, at.y as u16);
        let line = log_err("read link line", logical_line(&self.terminal, y, size.rows))?;
        let index = line.iter().position(|cell| cell.y == y && (cell.x..cell.x + cell.width).contains(&x))?;
        if let Some(uri) = &line[index].uri {
            let same = |cell: &LineCell| cell.uri.as_ref() == Some(uri);
            let first = line[..index].iter().rposition(|cell| !same(cell)).map_or(0, |i| i + 1);
            let last = line[index..].iter().position(|cell| !same(cell)).map_or(line.len(), |i| index + i);
            return Some(Link { target: LinkTarget::Url(uri.clone()), spans: spans(&line[first..last]) });
        }
        let cwd = self.cwd();
        let home = runode_paths::Dirs::from_env().home;
        let find_in = |cells: &[LineCell]| {
            let index = cells.iter().position(|cell| cell.y == y && (cell.x..cell.x + cell.width).contains(&x))?;
            let mut chars = Vec::new();
            let mut owners = Vec::new();
            for (i, cell) in cells.iter().enumerate() {
                for c in cell.text.chars() {
                    chars.push(c);
                    owners.push(i);
                }
            }
            let pointer = owners.iter().position(|&owner| owner == index)?;
            let (range, target) = find(&chars, pointer, |text| resolve_path(text, cwd.as_deref(), home.as_deref()))?;
            Some(Link { target, spans: spans(&cells[owners[range.start]..=owners[range.end - 1]]) })
        };
        // 接上硬换行的续行后认不出（比如本来就到行尾为止的路径接上了下一行的字）时，只在这一行里找。
        let joined = log_err("read hard-wrapped lines", join_hard_wraps(&self.terminal, &line, size.rows)).flatten();
        joined.as_deref().and_then(find_in).or_else(|| find_in(&line))
    }
}

/// Claude Code 这类程序自己按宽度折行：一行写满到最后一列，下一行先空几格缩进再接着写，中间是硬换行，
/// 终端不知道它们是一行。`line` 的头尾这样接着上下的逻辑行时，把它们接上并去掉续行开头的缩进；
/// 没有可接的时为 `None`。
fn join_hard_wraps(terminal: &Terminal<'_, '_>, line: &[LineCell], rows: u16) -> Result<Option<Vec<LineCell>>> {
    // 逻辑行的每一列都在 `cells` 里，最后一格就在最后一列。
    let ends_in_link = |cells: &[LineCell]| cells.last().is_some_and(|cell| cell.text.chars().all(link_char));
    let indent = |cells: &[LineCell]| cells.iter().take_while(|cell| cell.text == " ").count();
    let continues = |cells: &[LineCell]| cells.get(indent(cells)).is_some_and(|cell| cell.text.chars().all(link_char));
    let mut cells = line.to_vec();
    let mut joined = false;
    while cells[0].y > 0 && continues(&cells) {
        let above = logical_line(terminal, cells[0].y - 1, rows)?;
        if !ends_in_link(&above) {
            break;
        }
        cells.drain(..indent(&cells));
        cells.splice(0..0, above);
        joined = true;
    }
    while let Some(next) = cells.last().map(|cell| cell.y + 1).filter(|&next| next < rows && ends_in_link(&cells)) {
        let below = logical_line(terminal, next, rows)?;
        if !continues(&below) {
            break;
        }
        let skip = indent(&below);
        cells.extend(below.into_iter().skip(skip));
        joined = true;
    }
    Ok(joined.then_some(cells))
}

/// 读出视口第 `y` 行所在的逻辑行：往上找到软换行的开头，往下接到不再软换行的那一行，只在视口里找。
fn logical_line(terminal: &Terminal<'_, '_>, y: u16, rows: u16) -> Result<Vec<LineCell>> {
    let cols = terminal.cols()?;
    let grid_ref = |x: u16, y: u16| terminal.grid_ref(Point::Viewport(PointCoordinate { x, y: u32::from(y) }));
    let row = |y: u16| -> Result<Row> { grid_ref(0, y)?.row() };
    let mut top = y;
    while top > 0 && row(top)?.is_wrap_continuation()? {
        top -= 1;
    }
    let mut bottom = y;
    while bottom + 1 < rows && row(bottom)?.is_wrapped()? {
        bottom += 1;
    }
    let mut cells = Vec::new();
    for y in top..=bottom {
        let linked = row(y)?.has_hyperlink()?;
        for x in 0..cols {
            let grid_ref = grid_ref(x, y)?;
            let cell = grid_ref.cell()?;
            let width = match cell.wide()? {
                CellWide::SpacerTail | CellWide::SpacerHead => continue,
                CellWide::Wide => 2,
                CellWide::Narrow => 1,
            };
            let text = if cell.has_text()? { graphemes(&grid_ref)? } else { " ".to_owned() };
            let uri = if linked && cell.has_hyperlink()? { hyperlink_uri(&grid_ref)? } else { None };
            cells.push(LineCell { x, y, width, text, uri });
        }
    }
    Ok(cells)
}

fn graphemes(grid_ref: &GridRef<'_>) -> Result<String> {
    let mut buf = ['\0'; 16];
    match grid_ref.graphemes(&mut buf) {
        Ok(len) => Ok(buf[..len].iter().collect()),
        // 组合字符多得放不下时只取基本字符。
        Err(Error::OutOfSpace { .. }) => Ok(char::from_u32(grid_ref.cell()?.codepoint()?).unwrap_or(' ').to_string()),
        Err(err) => Err(err),
    }
}

fn hyperlink_uri(grid_ref: &GridRef<'_>) -> Result<Option<String>> {
    let mut buf = vec![0; 2048];
    let len = match grid_ref.hyperlink_uri(&mut buf) {
        Err(Error::OutOfSpace { required }) if required > buf.len() => {
            buf.resize(required, 0);
            grid_ref.hyperlink_uri(&mut buf)?
        }
        result => result?,
    };
    Ok((len > 0).then(|| String::from_utf8_lossy(&buf[..len]).into_owned()))
}

/// 一行里每段相邻的格子合成一段。
fn spans(cells: &[LineCell]) -> Vec<LinkSpan> {
    let mut spans: Vec<LinkSpan> = Vec::new();
    for cell in cells {
        match spans.last_mut() {
            Some(span) if span.y == cell.y => span.x.end = cell.x + cell.width,
            _ => spans.push(LinkSpan { y: cell.y, x: cell.x..cell.x + cell.width }),
        }
    }
    spans
}

/// 在 `chars` 里找盖住第 `pointer` 个字的网址或路径，返回它在 `chars` 里的范围和打开的目标。
/// `resolve` 把像路径的文字解析成存在的路径，不存在时为 `None`。
fn find(
    chars: &[char],
    pointer: usize,
    resolve: impl Fn(&str) -> Option<PathBuf>,
) -> Option<(Range<usize>, LinkTarget)> {
    if !link_char(chars[pointer]) {
        return None;
    }
    let start = chars[..pointer].iter().rposition(|&c| !link_char(c)).map_or(0, |i| i + 1);
    let end = chars[pointer..].iter().position(|&c| !link_char(c)).map_or(chars.len(), |i| pointer + i);
    let mut range = trim(chars, start..end);
    // 网址从协议名开始，前面粘着的（比如 `url=`）不算。
    if let Some(scheme) = scheme_start(&chars[range.clone()]) {
        range.start += scheme;
        if !range.contains(&pointer) {
            return None;
        }
        let url: String = chars[range.clone()].iter().collect();
        return Some((range, LinkTarget::Url(url)));
    }
    if !range.contains(&pointer) {
        return None;
    }
    let resolve_in = |range: Range<usize>| {
        let text: String = chars[range].iter().collect();
        resolve(strip_location(&text))
            // git diff 的 `a/`、`b/` 前缀。
            .or_else(|| {
                text.strip_prefix("a/")
                    .or_else(|| text.strip_prefix("b/"))
                    .and_then(|rest| resolve(strip_location(rest)))
            })
    };
    if let Some(path) = resolve_in(range.clone()) {
        return Some((range, LinkTarget::Path(path)));
    }
    // `S=路径`、`--out=路径` 这样前面粘着变量名或选项的，从等号后面算。
    range.start += chars[range.clone()].iter().position(|&c| c == '=')? + 1;
    if !range.contains(&pointer) {
        return None;
    }
    resolve_in(range.clone()).map(|path| (range, LinkTarget::Path(path)))
}

/// 能出现在网址和路径里的字。中日韩这类宽字符不算，免得把紧挨着路径的中文也吞进去。
fn link_char(c: char) -> bool {
    match c {
        'a'..='z' | 'A'..='Z' | '0'..='9' => true,
        '-' | '_' | '.' | '~' | '/' | ':' | '@' | '%' | '+' | '=' | '#' | '?' | '&' | ',' | '!' | '$' | '*' | ';' => {
            true
        }
        _ => !c.is_ascii() && c.is_alphanumeric() && crate::cell_width(c) == 1,
    }
}

/// 去掉句末的标点：结尾的 `.,:;!?`，开头的 `,:;!?`（开头的 `.` 是 `./`、`../` 的一部分）。
fn trim(chars: &[char], mut range: Range<usize>) -> Range<usize> {
    while range.end > range.start && matches!(chars[range.end - 1], '.' | ',' | ':' | ';' | '!' | '?') {
        range.end -= 1;
    }
    while range.start < range.end && matches!(chars[range.start], ',' | ':' | ';' | '!' | '?') {
        range.start += 1;
    }
    range
}

/// `chars` 里 `协议名://` 开头的位置；协议名由字母开头，后面是字母、数字和 `+.-`。
fn scheme_start(chars: &[char]) -> Option<usize> {
    let colon = chars.windows(3).position(|w| w == [':', '/', '/'])?;
    let start = chars[..colon]
        .iter()
        .rposition(|&c| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')))
        .map_or(0, |i| i + 1);
    let scheme = &chars[start..colon];
    let host = chars.get(colon + 3).is_some_and(|&c| c != '/' || scheme == ['f', 'i', 'l', 'e']);
    (scheme.first().is_some_and(char::is_ascii_alphabetic) && host).then_some(start)
}

/// 去掉路径后面的位置：`:行`、`:行:列`、`#L行`。
fn strip_location(text: &str) -> &str {
    let text = match text.rfind("#L") {
        Some(i) if text[i + 2..].bytes().all(|b| b.is_ascii_digit() || b == b'-' || b == b'L') => &text[..i],
        _ => text,
    };
    let mut text = text;
    for _ in 0..2 {
        match text.rsplit_once(':') {
            Some((head, tail)) if !head.is_empty() && !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => {
                text = head;
            }
            _ => break,
        }
    }
    text
}

/// 像路径的文字解析成存在的路径：`~/` 开头的按家目录 `home`，相对路径按 `cwd`。
fn resolve_path(text: &str, cwd: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    if text.is_empty() || text == "~" {
        return None;
    }
    let path = match text.strip_prefix("~/") {
        Some(rest) => home?.join(rest),
        None if text.starts_with('/') => PathBuf::from(text),
        None => cwd?.join(text),
    };
    path.exists().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 在 `line` 里找盖住 `needle` 第一个字的链接；`exists` 里的相对路径算存在，解析成 `/cwd/` 下。
    fn found(line: &str, needle: &str, exists: &[&str]) -> Option<(String, LinkTarget)> {
        let chars: Vec<char> = line.chars().collect();
        let byte = line.find(needle).unwrap();
        let pointer = line[..byte].chars().count();
        let (range, target) =
            find(&chars, pointer, |text| exists.contains(&text).then(|| Path::new("/cwd").join(text)))?;
        Some((chars[range].iter().collect(), target))
    }

    fn url(s: &str) -> LinkTarget {
        LinkTarget::Url(s.to_owned())
    }

    fn path(s: &str) -> LinkTarget {
        LinkTarget::Path(Path::new("/cwd").join(s))
    }

    #[test]
    fn finds_a_url_without_the_trailing_punctuation() {
        let line = "see https://example.com/a?b=1&c=2. Then";
        assert_eq!(
            found(line, "example", &[]),
            Some(("https://example.com/a?b=1&c=2".into(), url("https://example.com/a?b=1&c=2")))
        );
        assert_eq!(found(line, "see", &[]), None);
    }

    #[test]
    fn a_url_starts_at_its_scheme() {
        let line = "url=http://localhost:3000/";
        assert_eq!(found(line, "local", &[]), Some(("http://localhost:3000/".into(), url("http://localhost:3000/"))));
        assert_eq!(found(line, "url", &[]), None);
    }

    #[test]
    fn finds_an_existing_path_with_a_line_number() {
        let line = "  ⎿  Updated src/main.rs:12:5, done";
        assert_eq!(found(line, "main", &["src/main.rs"]), Some(("src/main.rs:12:5".into(), path("src/main.rs"))));
        assert_eq!(found(line, "main", &[]), None);
        assert_eq!(found(line, "Updated", &["src/main.rs"]), None);
    }

    #[test]
    fn brackets_quotes_and_wide_characters_end_a_path() {
        assert_eq!(
            found("⏺ Update(src/lib.rs)", "lib", &["src/lib.rs"]),
            Some(("src/lib.rs".into(), path("src/lib.rs")))
        );
        assert_eq!(found("改了`a.rs`和b.rs文件", "b.rs", &["b.rs"]), Some(("b.rs".into(), path("b.rs"))));
        assert_eq!(found("\"./x/y\"", "x", &["./x/y"]), Some(("./x/y".into(), path("./x/y"))));
    }

    #[test]
    fn a_path_starts_after_an_assignment() {
        let line = "$ S=/tmp/x/scratchpad; cat";
        assert_eq!(
            found(line, "x/", &["/tmp/x/scratchpad"]),
            Some(("/tmp/x/scratchpad".into(), path("/tmp/x/scratchpad")))
        );
        assert_eq!(found(line, "S=", &["/tmp/x/scratchpad"]), None);
        assert_eq!(found("--out=a.rs", "a.rs", &["a.rs"]), Some(("a.rs".into(), path("a.rs"))));
    }

    #[test]
    fn drops_the_git_diff_prefix() {
        assert_eq!(
            found("+++ b/crates/x.rs", "crates", &["crates/x.rs"]),
            Some(("b/crates/x.rs".into(), path("crates/x.rs")))
        );
    }

    #[test]
    fn strips_locations() {
        assert_eq!(strip_location("a.rs:12:5"), "a.rs");
        assert_eq!(strip_location("a.rs:12"), "a.rs");
        assert_eq!(strip_location("a.rs#L3-L9"), "a.rs");
        assert_eq!(strip_location("a:b"), "a:b");
        assert_eq!(strip_location("12"), "12");
    }
}
