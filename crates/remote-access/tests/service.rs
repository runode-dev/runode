//! `Service` 跟着调用方要的端口和名字开、关、重开监听，对外看监听方的状态文件。

use std::{
    net::TcpListener,
    sync::Arc,
    time::{Duration, Instant},
};

use runode_paths::Dirs;
use runode_remote_access::{Bind, Options, Service, listener_status};

/// 等状态文件里的名字变成 `name`（`None` 是关着），最多等几秒。
fn wait_for(dirs: &Dirs, name: Option<&str>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = listener_status(dirs).unwrap();
        if status.as_ref().map(|status| status.host_name.as_str()) == name {
            return;
        }
        assert!(Instant::now() < deadline, "the listener shows {status:?}, not {name:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn a_new_name_restarts_the_listener_under_that_name() {
    let root = std::env::temp_dir().join(format!("rra-service-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let dirs = Dirs::from_vars(|_| Some(root.clone().into()));
    // 要一个固定的端口：0 会被系统换成别的，和要的对不上就一直重开。
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let service = Service::start(Options {
        dirs: dirs.clone(),
        port: 0,
        bind: Bind::Loopback,
        host_name: None,
        advertise: false,
        connect: Arc::new(|| Err(std::io::Error::other("no host"))),
    })
    .unwrap();
    service.set(Some(port), Some("书房的 Mac".into()));
    wait_for(&dirs, Some("书房的 Mac"));
    service.set(Some(port), Some("客厅的 Mac".into()));
    wait_for(&dirs, Some("客厅的 Mac"));
    service.set(None, Some("客厅的 Mac".into()));
    wait_for(&dirs, None);
    drop(service);
    std::fs::remove_dir_all(&root).unwrap();
}
