//! 系统库（C 库、Objective-C 框架）调进来的回调：panic 不能穿过它们展开。

/// 跑回调 `body`。panic 在这里停下、记成 error，回调就当什么也没做地返回；`body` 半途 panic 后留下的状态要照样
/// 能用。
pub(crate) fn in_callback(callback: &str, body: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)).is_err() {
        tracing::error!("the {callback} callback panicked");
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn callbacks_swallow_panics() {
        let ran = Cell::new(false);
        in_callback("test", || {
            ran.set(true);
            panic!("the callback failed");
        });
        assert!(ran.get());
    }
}
