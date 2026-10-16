//! 按几格字符转的圈：标签、侧栏上工作中的 agent 和看板的提示条都用。

use std::time::Duration;

use gpui::{Animation, AnimationExt as _, App, Context, ElementId, Entity, Render, Window, div, prelude::*};

/// 转圈单独是一个视图：动画每动一格只通知它自己，GPUI 重画它和它外面的几层，但不当成外层视图（桌面 app
/// 的 `WindowView`）的状态变了，缓存的侧栏、标签栏和右侧面板不跟着重画。
struct Spinner {
    frames: &'static [&'static str],
    frame_time: Duration,
}

impl Render for Spinner {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let (frames, frame_time) = (self.frames, self.frame_time);
        // 没有帧时没东西可画；每帧停零秒时动画周期是零、帧率没有上限，只画第一帧。
        if frames.is_empty() || frame_time.is_zero() {
            return div().children(frames.first().copied()).into_any_element();
        }
        let period = frame_time * frames.len() as u32;
        div()
            .with_animation(
                "spinner",
                // 每格只重画一次。
                Animation::new(period).repeat_synced().with_max_fps(1. / frame_time.as_secs_f32()),
                move |spinner, delta| {
                    let frame = (delta * frames.len() as f32) as usize;
                    spinner.child(frames[frame.min(frames.len() - 1)])
                },
            )
            .into_any_element()
    }
}

/// 在 `id` 处画一个按 `frames` 转的圈，每格停 `frame_time`，见 `SpinnerSlot`。
pub fn spinner(id: impl Into<ElementId>, frames: &'static [&'static str], frame_time: Duration) -> impl IntoElement {
    SpinnerSlot { id: id.into(), frames, frame_time }
}

/// 在 `id` 处画 `Spinner`，连续几帧都画着时沿用同一个（存在元素状态里）。不用 `use_keyed_state`：
/// 它把状态的通知转给外层视图，转圈每动一格又成了外层的状态变了。
#[derive(IntoElement)]
struct SpinnerSlot {
    id: ElementId,
    frames: &'static [&'static str],
    frame_time: Duration,
}

impl RenderOnce for SpinnerSlot {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { id, frames, frame_time } = self;
        let spinner = window.with_global_id(id, |id, window| {
            window.with_element_state(id, |state: Option<Entity<Spinner>>, _| {
                let spinner = state.unwrap_or_else(|| cx.new(|_| Spinner { frames, frame_time }));
                (spinner.clone(), spinner)
            })
        });
        // 同一个位置上换了一种转法（agent 换了种类）时换成新的几格。
        spinner.update(cx, |spinner, _| {
            spinner.frames = frames;
            spinner.frame_time = frame_time;
        });
        spinner
    }
}
