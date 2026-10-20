//! 预览栏的编辑模式：普通标签里的文本文件换成多行输入框来改，⌘S 存回磁盘。改过没存的标签名后面
//! 带一个圆点，关掉它或退出编辑之前先问一句；文件在磁盘上被别人改过时存之前也先问。
//!
//! 从 Dock 退出、注销、关机这类系统发起的退出弹不了框，没存的修改会丢。

use std::{fs, io, path::Path, time::SystemTime};

use gpui::{
    Context, Entity, Focusable as _, KeyDownEvent, PromptLevel, SharedString, Subscription, Window, prelude::*, px,
};
use runode_preview::MAX_TEXT_BYTES;

use super::{BODY_PADDING, Loaded, Preview, PreviewPanel, ROW_EXTRA_HEIGHT, file_stamp};
use crate::ui::{
    textarea::{Textarea, TextareaEvent, TextareaState},
    theme::ActiveTheme as _,
};

pub(super) struct Editor {
    area: Entity<TextareaState>,
    /// 读进来或上次存下的文字，和输入框里的比就知道改没改。
    saved: String,
    dirty: bool,
    /// 文件原来用 `\r\n` 换行、开头带 BOM：输入框里一律是 `\n`，存的时候换回去。
    crlf: bool,
    bom: bool,
    /// 读进来或上次存下时文件的大小和修改时间，存之前和磁盘上的比。
    stamp: Option<(u64, SystemTime)>,
    /// 正在存：这时再按 ⌘S 不管，免得两次写的先后乱了，旧的盖掉新的。
    saving: bool,
    _changed: Subscription,
}

/// 读进来要编辑的文件：原文去掉 BOM、换行统一成 `\n` 之前记下的样子，和读时的大小、修改时间。
struct Source {
    text: String,
    crlf: bool,
    bom: bool,
    stamp: Option<(u64, SystemTime)>,
}

/// 读 `path` 的原文；太大、含 NUL 或不是 UTF-8 的不能编辑。
fn read_source(path: &Path) -> io::Result<Source> {
    let stamp = file_stamp(path);
    let bytes = fs::read(path)?;
    if bytes.len() > MAX_TEXT_BYTES || bytes.contains(&0) {
        return Err(io::Error::other("not an editable text file"));
    }
    let text = String::from_utf8(bytes).map_err(io::Error::other)?;
    let (bom, text) = match text.strip_prefix('\u{feff}') {
        Some(rest) => (true, rest.to_owned()),
        None => (false, text),
    };
    Ok(Source { crlf: text.contains("\r\n"), bom, text, stamp })
}

/// 输入框里的文字存回磁盘的字节：换行和 BOM 照文件原来的样子。
fn encode(text: &str, crlf: bool, bom: bool) -> Vec<u8> {
    let text = if crlf { text.replace('\n', "\r\n") } else { text.to_owned() };
    [if bom { "\u{feff}" } else { "" }, &text].concat().into_bytes()
}

/// 弹框报错，只有一个「好」。
fn report(title: &str, err: &io::Error, window: &mut Window, cx: &mut Context<PreviewPanel>) {
    // 不等回答：框关掉就完了。
    drop(window.prompt(PromptLevel::Critical, title, Some(&err.to_string()), &[&*rust_i18n::t!("git.ok")], cx));
}

impl Preview {
    /// 在编辑，改过还没存。
    pub(super) fn dirty(&self) -> bool {
        self.editor.as_ref().is_some_and(|editor| editor.dirty)
    }

    /// 能进编辑模式：普通标签里读全了的文本。
    pub(super) fn editable(&self) -> bool {
        self.diff.is_none() && matches!(self.content, Some(Loaded::Text { truncated: false, .. }))
    }
}

impl PreviewPanel {
    /// 当前标签进出编辑模式。进的时候固定标签，免得打开别的文件时被换掉；改过没存时退出先问。
    pub(super) fn toggle_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preview) = self.preview_mut() else {
            return;
        };
        if preview.editor.is_none() {
            if preview.editable() {
                preview.pinned = true;
                self.start_editing(window, cx);
            }
            return;
        }
        if !preview.dirty() {
            preview.editor = None;
            window.focus(&self.focus, cx);
            cx.notify();
            return;
        }
        let area = preview.editor.as_ref().map(|editor| editor.area.clone());
        let answer = self.ask_discard(1, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() == Some(0) {
                this.update_in(cx, |this, window, cx| {
                    if let Some(preview) =
                        this.preview_mut().filter(|preview| preview.editor.as_ref().map(|e| e.area.clone()) == area)
                    {
                        preview.editor = None;
                        window.focus(&this.focus, cx);
                        cx.notify();
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// 在后台读当前标签的原文，读好了换成输入框、把焦点给它。读不了时报错，不进编辑。
    fn start_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(preview) = self.preview() else {
            return;
        };
        let (ws, path) = (self.ws, preview.path.clone());
        let job = cx.background_spawn({
            let path = path.clone();
            async move { read_source(&path) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let source = job.await;
            this.update_in(cx, |this, window, cx| {
                let source = match source {
                    Ok(source) => source,
                    Err(err) => {
                        report(&rust_i18n::t!("preview.edit_failed"), &err, window, cx);
                        return;
                    }
                };
                // 读的期间切走了的不抢焦点。
                let shown = this.ws == ws && this.preview().is_some_and(|tab| tab.path == path && tab.diff.is_none());
                // 读的期间标签关了、又进过一次编辑的不管。
                let tabs = this.workspaces.get_mut(&ws).map_or(&mut [][..], |tabs| &mut tabs.tabs[..]);
                let Some(preview) =
                    tabs.iter_mut().find(|tab| tab.path == path && tab.diff.is_none() && tab.editor.is_none())
                else {
                    return;
                };
                let name = preview.name();
                let area = cx.new(|cx| {
                    let mut area = TextareaState::new(cx);
                    area.load_text(source.text.clone(), cx);
                    area.fill_height(cx);
                    area.set_label(name, cx);
                    area
                });
                let changed = cx.subscribe(&area, |this, area, event: &TextareaEvent, cx| {
                    if let TextareaEvent::Changed = event
                        && let Some(editor) = this.editor_mut(&area)
                    {
                        editor.dirty = area.read(cx).text() != editor.saved;
                        cx.notify();
                    }
                });
                if shown {
                    window.focus(&area.focus_handle(cx), cx);
                }
                // 和输入框里换行统一过的文字比，`\r\n` 的文件刚读进来不算改过。
                let saved = area.read(cx).text().to_owned();
                preview.editor = Some(Editor {
                    area,
                    saved,
                    dirty: false,
                    crlf: source.crlf,
                    bom: source.bom,
                    stamp: source.stamp,
                    saving: false,
                    _changed: changed,
                });
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 输入框 `area` 所在的编辑器，在哪个 workspace 都找。
    fn editor_mut(&mut self, area: &Entity<TextareaState>) -> Option<&mut Editor> {
        self.workspaces
            .values_mut()
            .flat_map(|tabs| &mut tabs.tabs)
            .filter_map(|tab| tab.editor.as_mut())
            .find(|editor| editor.area == *area)
    }

    /// 问要不要放弃 `count` 个标签里没存的修改，答 0 是放弃。
    pub(super) fn ask_discard(
        &self,
        count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> futures::channel::oneshot::Receiver<usize> {
        window.prompt(
            PromptLevel::Warning,
            &rust_i18n::t!("preview.unsaved_title", count = count),
            Some(&rust_i18n::t!("preview.unsaved_detail")),
            &[&*rust_i18n::t!("preview.discard_edits"), &*rust_i18n::t!("git.cancel")],
            cx,
        )
    }

    /// 把编辑器 `area` 里的文字存回它的文件。文件在磁盘上比读进来、上次存下时变过（别人改了、删了）先问
    /// 要不要覆盖；写是先写临时文件再换上（`replace_file`），写不成时原文件不动、报错。
    fn save(&mut self, area: Entity<TextareaState>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self
            .workspaces
            .values()
            .flat_map(|tabs| &tabs.tabs)
            .find(|tab| tab.editor.as_ref().is_some_and(|editor| editor.area == area))
            .map(|tab| tab.path.clone())
        else {
            return;
        };
        let Some(editor) = self.editor_mut(&area).filter(|editor| !editor.saving) else {
            return;
        };
        editor.saving = true;
        let text = area.read(cx).text().to_owned();
        let (bytes, known) = (encode(&text, editor.crlf, editor.bom), editor.stamp);
        cx.spawn_in(window, async move |this, cx| {
            let current = {
                let path = path.clone();
                crate::fs_probe::off_thread(move || file_stamp(&path), None).await
            };
            let overwrite = current == known || {
                let answer = this.update_in(cx, |_, window, cx| {
                    window.prompt(
                        PromptLevel::Warning,
                        &rust_i18n::t!("preview.changed_on_disk_title"),
                        Some(&rust_i18n::t!("preview.changed_on_disk_detail")),
                        &[&*rust_i18n::t!("preview.overwrite"), &*rust_i18n::t!("git.cancel")],
                        cx,
                    )
                });
                match answer {
                    Ok(answer) => answer.await.ok() == Some(0),
                    Err(_) => false,
                }
            };
            let written = match overwrite {
                true => Some(
                    cx.background_spawn(async move {
                        runode_paths::replace_file(&path, &bytes).map(|()| file_stamp(&path))
                    })
                    .await,
                ),
                false => None,
            };
            this.update_in(cx, |this, window, cx| {
                let Some(editor) = this.editor_mut(&area) else {
                    return;
                };
                editor.saving = false;
                match written {
                    Some(Ok(stamp)) => {
                        editor.dirty = area.read(cx).text() != text;
                        editor.saved = text;
                        editor.stamp = stamp;
                        cx.notify();
                    }
                    Some(Err(err)) => report(&rust_i18n::t!("preview.save_failed"), &err, window, cx),
                    None => {}
                }
            })
            .ok();
        })
        .detach();
    }

    /// 编辑模式下的正文：撑满的输入框，用终端的字体和预览字号。⌘S 存，Tab 插入制表符。
    pub(super) fn render_editor(
        &self,
        editor: &Editor,
        font: SharedString,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let font_size = self.input.font_size;
        let area = editor.area.clone();
        Textarea::new(&editor.area)
            .id("preview-editor")
            .flex_1()
            .min_h_0()
            .border_0()
            .rounded_none()
            .bg(cx.theme().background)
            .px(px(12.))
            .py(px(BODY_PADDING))
            .font_family(font)
            .line_height(px(font_size + ROW_EXTRA_HEIGHT))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                let mods = &event.keystroke.modifiers;
                let plain = !mods.control && !mods.alt && !mods.shift;
                match event.keystroke.key.as_str() {
                    "s" if plain && mods.platform => this.save(area.clone(), window, cx),
                    "tab" if plain && !mods.platform => area.update(cx, |area, cx| area.insert("\t", cx)),
                    _ => return,
                }
                cx.stop_propagation();
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 存回去的字节保留文件原来的换行和 BOM，只改了一处的文件不会整篇都变。
    #[test]
    fn saving_keeps_line_endings_and_bom() {
        let dir = std::env::temp_dir().join(format!("runode-edit-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        for original in ["\u{feff}one\r\ntwo\r\n", "one\ntwo", ""] {
            fs::write(&path, original).unwrap();
            let source = read_source(&path).unwrap();
            // 输入框里的换行一律是 `\n`。
            let shown = source.text.replace("\r\n", "\n");
            assert_eq!(encode(&shown, source.crlf, source.bom), original.as_bytes());
        }
        fs::write(&path, b"a\0b").unwrap();
        assert!(read_source(&path).is_err());
        fs::remove_dir_all(&dir).ok();
    }
}
