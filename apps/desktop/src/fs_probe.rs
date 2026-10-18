//! 只看一下文件系统的小事（stat、解符号链接、是不是文件或目录）放到单独的线程上做，界面线程上 await 结果：
//! 文件在挂死的网络卷、autofs 上时这些调用会一直卡着，不能在界面线程上做，也不占后台线程池，免得反复触发
//! 几次就把池耗光，连带 git 扫描和预览加载都停下。

/// 在单独的线程上做 `f`，等它的结果；起不了线程时是 `fallback`。
pub(crate) async fn off_thread<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static, fallback: T) -> T {
    let (tx, rx) = futures::channel::oneshot::channel();
    let _ = std::thread::Builder::new().name("fs-probe".into()).spawn(move || {
        let _ = tx.send(f());
    });
    rx.await.unwrap_or(fallback)
}
