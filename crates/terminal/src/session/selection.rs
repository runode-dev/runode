//! 选区：鼠标拖选（libghostty 的手势状态机）、键盘调整、全选，以及取出选中的和整屏的文字。

use std::time::{Duration, Instant};

use libghostty_vt::{
    fmt::{Format, Formatter, FormatterOptions},
    screen::GridRef,
    selection::{
        Adjustment, FormatOptions,
        gesture::{self, AutoscrollTickEvent, DragEvent, Geometry, Gesture, PressEvent, ReleaseEvent},
    },
    terminal::{Point, PointSpace},
};
use runode_shared_types::{grid::GridPoint, input::SelectionAdjust};

use super::{Session, log_err};

/// 鼠标选区：libghostty 的手势状态机，加上各类事件复用的对象。
pub(super) struct Selecting {
    gesture: Gesture<'static>,
    press: PressEvent<'static>,
    drag: DragEvent<'static>,
    release: ReleaseEvent<'static>,
    autoscroll: AutoscrollTickEvent<'static>,
    /// 按下事件的时间基准，手势靠两次按下的间隔判断双击、三击。
    epoch: Instant,
    /// 拖动时指针最后的位置和是否选矩形块，自动滚动时沿用。
    pointer: (GridPoint, bool),
}

impl Selecting {
    pub(super) fn new() -> libghostty_vt::error::Result<Self> {
        Ok(Self {
            gesture: Gesture::new()?,
            press: PressEvent::new()?,
            drag: DragEvent::new()?,
            release: ReleaseEvent::new()?,
            autoscroll: AutoscrollTickEvent::new()?,
            epoch: Instant::now(),
            pointer: Default::default(),
        })
    }
}

impl Session {
    pub fn has_selection(&self) -> bool {
        self.terminal.selection().is_ok_and(|s| s.is_some())
    }

    /// 左键按下。手势按两次按下的间隔和距离数次数：单击清掉选区、从这里开始拖，
    /// 双击选词，三击选行。`repeat_interval` 是算作连击的最长间隔。
    pub fn select_press(&mut self, at: GridPoint, repeat_interval: Duration) {
        let (x, y) = self.surface_position(at);
        let size = self.size.get();
        let cell = self.viewport_cell(at);
        let s = &mut self.selecting;
        let result = self.terminal.grid_ref(Point::Viewport(cell)).and_then(|grid_ref| {
            let selection = s
                .press
                .set_repeat_distance(f64::from(size.cell_width_px))?
                .set_repeat_interval(repeat_interval)?
                .set_time(s.epoch.elapsed())?
                .set_position(x, y)?
                .apply(&mut s.gesture, &self.terminal, grid_ref)?;
            self.terminal.set_selection(selection.as_ref())?;
            Ok(())
        });
        log_err("selection press", result);
    }

    /// 按住左键拖动，扩展选区；`rectangle` 时选矩形块。返回是否拖到了网格上下边以外，
    /// 这时要定时调用 `select_autoscroll` 滚动视口。
    pub fn select_drag(&mut self, at: GridPoint, rectangle: bool) -> bool {
        let (x, y) = self.surface_position(at);
        let geometry = self.gesture_geometry();
        let cell = self.viewport_cell(at);
        let s = &mut self.selecting;
        s.pointer = (at, rectangle);
        let result = self.terminal.grid_ref(Point::Viewport(cell)).and_then(|grid_ref| {
            let selection = s.drag.set_rectangle(rectangle)?.set_position(x, y)?.apply(
                &mut s.gesture,
                &self.terminal,
                grid_ref,
                geometry,
            )?;
            self.terminal.set_selection(selection.as_ref())?;
            Ok(s.gesture.autoscroll(&self.terminal)? != gesture::Autoscroll::None)
        });
        log_err("selection drag", result).unwrap_or(false)
    }

    /// 拖到网格外时的定时调用：视口滚一行，再按最后的指针位置扩展选区。返回这次是否滚动了。
    pub fn select_autoscroll(&mut self) -> bool {
        let (at, rectangle) = self.selecting.pointer;
        let (x, y) = self.surface_position(at);
        let geometry = self.gesture_geometry();
        let cell = self.viewport_cell(at);
        let s = &mut self.selecting;
        let result = s.gesture.autoscroll(&self.terminal).and_then(|autoscroll| {
            if autoscroll == gesture::Autoscroll::None {
                return Ok(false);
            }
            let selection = s.autoscroll.set_rectangle(rectangle)?.set_position(x, y)?.apply(
                &mut s.gesture,
                &self.terminal,
                cell,
                geometry,
            )?;
            // 没有结果说明起点已不在当前屏幕上（比如切到了备用屏幕），原有选区保持不动。
            if let Some(selection) = &selection {
                self.terminal.set_selection(Some(selection))?;
            }
            Ok(selection.is_some())
        });
        log_err("selection autoscroll", result).unwrap_or(false)
    }

    /// 松开左键，结束这次拖动；选区留着。
    pub fn select_release(&mut self, at: GridPoint) {
        let size = self.size.get();
        let inside = (0. ..f32::from(size.cols)).contains(&at.x) && (0. ..f32::from(size.rows)).contains(&at.y);
        let cell = self.viewport_cell(at);
        let grid_ref = inside.then(|| self.terminal.grid_ref(Point::Viewport(cell)).ok()).flatten();
        let s = &mut self.selecting;
        log_err("selection release", s.release.apply(&mut s.gesture, &self.terminal, grid_ref));
    }

    /// 选区的纯文本：软换行处接起来，行尾空白去掉。没有选区时为 `None`。
    pub fn selection_text(&self) -> Option<String> {
        let options = FormatOptions::new().with_emit_format(Format::Plain).with_unwrap(true).with_trim(true);
        log_err("selection format", self.terminal.format_selection_alloc(None, options))
            .flatten()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    /// 选中屏幕和回滚历史里的全部内容。
    pub fn select_all(&mut self) {
        let result =
            self.terminal.select_all().and_then(|selection| self.terminal.set_selection(selection.as_ref()).map(drop));
        log_err("select all", result);
    }

    /// 用键盘移动选区的终点，并让终点留在视野里。没有选区时返回 `false`，按键照常交给程序。
    pub fn adjust_selection(&mut self, adjustment: SelectionAdjust) -> bool {
        let adjustment = match adjustment {
            SelectionAdjust::Left => Adjustment::Left,
            SelectionAdjust::Right => Adjustment::Right,
            SelectionAdjust::Up => Adjustment::Up,
            SelectionAdjust::Down => Adjustment::Down,
            SelectionAdjust::PageUp => Adjustment::PageUp,
            SelectionAdjust::PageDown => Adjustment::PageDown,
            SelectionAdjust::Home => Adjustment::Home,
            SelectionAdjust::End => Adjustment::End,
        };
        let result = (|| {
            let Some(mut selection) = self.terminal.selection()? else {
                return Ok(None);
            };
            selection.adjust(&self.terminal, adjustment)?;
            self.terminal.set_selection(Some(&selection))?;
            self.reveal_row(&selection.end()).map(Some)
        })();
        match log_err("selection adjust", result).flatten() {
            Some(row) => {
                if let Some(row) = row {
                    self.scroll_to_row(row);
                }
                true
            }
            None => false,
        }
    }

    /// 把视口滚到选区开头；没有选区时什么都不做。
    pub fn scroll_to_selection(&mut self) {
        let result = (|| {
            let Some(selection) = self.terminal.selection()? else {
                return Ok(None);
            };
            self.reveal_row(&selection.start())
        })();
        if let Some(row) = log_err("scroll to selection", result).flatten() {
            self.scroll_to_row(row);
        }
    }

    /// `grid_ref` 不在视口里时它在整个屏幕（含回滚历史）中的行号；已经看得到时为 `None`。
    fn reveal_row(&self, grid_ref: &GridRef<'_>) -> libghostty_vt::error::Result<Option<u32>> {
        if self.terminal.point_from_grid_ref(grid_ref, PointSpace::Viewport)?.is_some() {
            return Ok(None);
        }
        Ok(self.terminal.point_from_grid_ref(grid_ref, PointSpace::Screen)?.map(|p| p.y))
    }

    /// 当前屏幕连同回滚历史的纯文本：软换行处接起来，行尾空白去掉。
    pub fn screen_text(&self) -> Option<String> {
        let options = FormatterOptions::new().with_format(Format::Plain).with_unwrap(true).with_trim(true);
        let result = Formatter::new(&self.terminal, options)
            .and_then(|mut formatter| formatter.format_alloc(None).map(|bytes| bytes.to_vec()));
        log_err("format screen", result).map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    fn gesture_geometry(&self) -> Geometry {
        let size = self.size.get();
        Geometry {
            columns: u32::from(size.cols.max(1)),
            cell_width: u32::from(size.cell_width_px.max(1)),
            padding_left: 0,
            screen_height: (u32::from(size.rows) * u32::from(size.cell_height_px)).max(1),
        }
    }
}
