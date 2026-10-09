//! `runode remote …`：不经宿主，对着临时目录里远程访问的文件跑。配对要监听方开着，这里开一个只听
//! 回环地址、接不到宿主的监听；配对本身（手机那一侧）由 `runode-remote-access` 自己的测试管。

mod common;

use std::{path::PathBuf, sync::Arc, thread, time::Duration};

use common::run;
use runode_cli::{Env, exit};
use runode_paths::Dirs;
use runode_protocol::remote::{PAIRING_TTL, PairingUri};
use runode_remote_access::{Bind, Listener, Options, PairingTicket, list_devices};

const PHONE: &str = "00112233445566778899aabbccddeeff";
const TABLET: &str = "0011ffeeddccbbaa9988776655443322";

/// 一个临时目录，`Env::dirs` 指向它。
struct Root(PathBuf);

impl Root {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("rnr-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }

    fn dirs(&self) -> Dirs {
        Dirs::from_vars(|_| Some(self.0.clone().into()))
    }

    fn env(&self) -> Env {
        Env { dirs: self.dirs(), build: "test".into(), ..Env::default() }
    }

    /// 照设备表的格式写两台设备。
    fn write_devices(&self) {
        let dir = self.dirs().remote_access_dir().unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let key = "BAEIDxYdJCsyOUBHTlVcY2pxeH-GjZSboqmwt77FzNPa4ejv9v0ECxIZICcuNTxDSlFYX2ZtdHuCiZCXnqWss7o";
        let json = format!(
            r#"{{"version":1,"devices":[
                {{"device_id":"{PHONE}","name":"Ethan 的 iPhone","public_key":"{key}","paired_at":1,"last_seen":2}},
                {{"device_id":"{TABLET}","name":"iPad","public_key":"{key}","paired_at":3,"last_seen":4}}]}}"#
        );
        std::fs::write(self.dirs().remote_access_devices_file().unwrap(), json).unwrap();
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn devices_are_listed_and_revoked_by_prefix() {
    let root = Root::new("devices");
    let env = root.env();
    let (code, out, _) = run("remote devices", &env);
    assert_eq!(code, exit::OK);
    assert!(out.contains("No paired devices"), "{out}");
    root.write_devices();
    let (code, out, _) = run("remote devices", &env);
    assert_eq!(code, exit::OK);
    assert!(out.starts_with("ID "), "{out}");
    assert!(out.contains(PHONE) && out.contains("Ethan 的 iPhone") && out.contains("iPad"), "{out}");
    let (_, json, _) = run("remote devices --json", &env);
    let json: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(json[1]["device_id"], TABLET);
    // 前缀对上两台时列出来，不撤销。
    let (code, _, err) = run("remote revoke 0011", &env);
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("matches 2 devices"), "{err}");
    let (code, out, _) = run("remote revoke 00112233", &env);
    assert_eq!(code, exit::OK);
    assert!(out.contains("Revoked Ethan 的 iPhone"), "{out}");
    let left = list_devices(&root.dirs()).unwrap();
    assert_eq!(left.iter().map(|device| device.name.as_str()).collect::<Vec<_>>(), ["iPad"]);
    let (code, _, err) = run(&format!("remote revoke {PHONE}"), &env);
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("no paired device"), "{err}");
}

#[test]
fn pairing_turns_remote_access_on_in_the_config_and_waits_for_the_listener() {
    let root = Root::new("off");
    let env = root.env();
    let pairing = thread::spawn(move || run("remote pair --addr 127.0.0.1", &env));
    // 命令改好配置后在等；监听方（这里是测试）读到配置起来，命令接着出二维码。
    let config = root.dirs().config_file().unwrap();
    for _ in 0..100 {
        if config.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(std::fs::read_to_string(&config).unwrap(), "remote-access = true\n");
    let _listener = Listener::start(Options {
        dirs: root.dirs(),
        port: 0,
        bind: Bind::Loopback,
        host_name: Some("测试的 Mac".into()),
        advertise: false,
        connect: Arc::new(|| Err(std::io::Error::other("no host in this test"))),
    })
    .unwrap();
    let file = root.dirs().remote_access_pairing_file().unwrap();
    for _ in 0..100 {
        if file.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let _replacement = PairingTicket::begin(&root.dirs(), PAIRING_TTL).unwrap();
    let (code, out, err) = pairing.join().unwrap();
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("replaced"), "{err}");
    assert!(out.starts_with("Remote access was off: set `remote-access = true` in "), "{out}");
    assert!(out.contains('▀'), "{out}");
}

#[test]
fn pairing_shows_a_code_and_a_link_and_waits() {
    let root = Root::new("pair");
    let listener = Listener::start(Options {
        dirs: root.dirs(),
        port: 0,
        bind: Bind::Loopback,
        host_name: Some("测试的 Mac".into()),
        advertise: false,
        connect: Arc::new(|| Err(std::io::Error::other("no host in this test"))),
    })
    .unwrap();
    let env = root.env();
    let pairing = thread::spawn(move || run("remote pair --addr 127.0.0.1", &env));
    // 等命令写好口令文件，再用另一次配对换掉它，命令就不再等了。
    let file = root.dirs().remote_access_pairing_file().unwrap();
    for _ in 0..100 {
        if file.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    let replacement = PairingTicket::begin(&root.dirs(), PAIRING_TTL).unwrap();
    let (code, out, err) = pairing.join().unwrap();
    assert_eq!(code, exit::FAILED);
    assert!(err.contains("replaced"), "{err}");
    assert!(out.contains('▀'), "{out}");
    let uri: PairingUri = out.lines().find(|line| line.starts_with("runode://")).unwrap().parse().unwrap();
    assert_eq!(uri.host_name, "测试的 Mac");
    assert_eq!(uri.port, listener.local_addrs()[0].port());
    assert_eq!(uri.addrs[0], "127.0.0.1".parse::<std::net::IpAddr>().unwrap());
    assert_ne!(uri.secret, replacement.secret());
    let status = runode_remote_access::listener_status(&root.dirs()).unwrap().unwrap();
    assert_eq!(uri.fingerprint.to_vec(), status.fingerprint.0);
}
