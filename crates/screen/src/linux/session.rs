//! 找出屏幕上这个用户登录着的图形会话，接过它的环境变量：采集进程由宿主拉起，宿主可能跑在 ssh 的登录或用户
//! 服务里，自己没有 `DISPLAY`、`WAYLAND_DISPLAY` 和会话总线的地址。
//!
//! 会话按 logind 记在 `SESSIONS_DIR` 里的文件找，谁都能读，不用 D-Bus；环境变量从这个会话里跑着的、本用户的
//! 进程的 `/proc/<pid>/environ` 读。没有 logind 的系统上退到本用户的 X 或 Wayland 的 socket。

use std::{
    collections::HashMap,
    ffi::OsString,
    fs,
    os::unix::{ffi::OsStringExt as _, fs::MetadataExt as _},
    path::{Path, PathBuf},
};

/// logind 给每个会话写一个文件的目录。
const SESSIONS_DIR: &str = "/run/systemd/sessions";
/// 本机 X 服务器的 socket 所在的目录，`X<n>` 对应显示 `:n`。
const X11_SOCKETS: &str = "/tmp/.X11-unix";

/// 要从会话里接过来的环境变量。
const IMPORTED: &[&str] = &[
    "DISPLAY",
    "XAUTHORITY",
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_SESSION_TYPE",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_DESKTOP",
    "XDG_SESSION_ID",
];

/// 图形会话用的显示协议。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    X11,
    Wayland,
}

/// 屏幕上的一个图形会话。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Session {
    pub(crate) kind: Kind,
    /// 接过来的环境变量，只含 `IMPORTED` 里的。
    pub(crate) env: HashMap<String, String>,
}

/// 本用户在屏幕上有图形会话。
pub(crate) fn exists() -> bool {
    find().is_some()
}

/// 本用户在屏幕上的图形会话：logind 记着的、本用户的 X11 或 Wayland 会话，活动的那个优先；没有 logind 时看本用户
/// 自己起的 X 或 Wayland 服务器。本进程的环境里已经有 `WAYLAND_DISPLAY` 或 `DISPLAY`、而且它指的服务器还在时就是它。
pub(crate) fn find() -> Option<Session> {
    if let Some(session) = from_own_env() {
        return Some(session);
    }
    // SAFETY: 没有参数。
    let uid = unsafe { libc::getuid() };
    from_logind(Path::new(SESSIONS_DIR), uid).or_else(|| from_sockets(uid))
}

/// 把会话的环境变量设进本进程，之后的 X、D-Bus、PipeWire 连接和 logind 的查询都按它们找。本进程已有的一律
/// 盖掉：经 ssh 起的进程带着 ssh 那个会话的 `XDG_SESSION_ID`、`XDG_SESSION_TYPE=tty`，开了 X 转发时还有指向
/// 转发的 `DISPLAY`，会话里没有 `DISPLAY` 时这个也去掉。只在还没开别的线程时调。
pub(crate) fn adopt(session: &Session) {
    if !session.env.contains_key("DISPLAY") {
        // SAFETY: 调用方保证本进程还没有别的线程，没人同时读写环境。
        unsafe { std::env::remove_var("DISPLAY") };
    }
    for (key, value) in &session.env {
        // SAFETY: 同上。
        unsafe { std::env::set_var(key, value) };
    }
}

fn from_own_env() -> Option<Session> {
    let env: HashMap<String, String> =
        IMPORTED.iter().filter_map(|key| Some(((*key).to_owned(), std::env::var(key).ok()?))).collect();
    let kind = kind_of(&env)?;
    // 宿主作为用户服务在登录桌面之后起的，继承了桌面导进用户管理器的变量，用户注销后它们还在。
    // SAFETY: 没有参数。
    let uid = unsafe { libc::getuid() };
    server_running(&env, kind, uid).then_some(Session { kind, env })
}

/// `env` 里 `kind` 的显示服务器还在监听。
fn server_running(env: &HashMap<String, String>, kind: Kind, uid: u32) -> bool {
    match kind {
        Kind::Wayland => {
            let Some(name) = env.get("WAYLAND_DISPLAY") else { return false };
            let runtime = env.get("XDG_RUNTIME_DIR").cloned().unwrap_or_else(|| format!("/run/user/{uid}"));
            // 绝对路径直接用，否则在运行目录里。
            listening(&Path::new(&runtime).join(name))
        }
        Kind::X11 => env.get("DISPLAY").and_then(|display| x_display_number(display)).is_some_and(x_listening),
    }
}

/// `:0`、`:1.0` 这样的本机显示的编号。
fn x_display_number(display: &str) -> Option<u32> {
    let number = display.strip_prefix(':')?;
    number.split('.').next()?.parse().ok()
}

/// 显示 `:number` 的 X 服务器在监听：Linux 上 X 先在抽象 socket 上听，有的只听抽象的，两处都试。
fn x_listening(number: u32) -> bool {
    use std::os::{linux::net::SocketAddrExt as _, unix::net::SocketAddr};

    let path = Path::new(X11_SOCKETS).join(format!("X{number}"));
    let name = path.as_os_str().as_encoded_bytes();
    SocketAddr::from_abstract_name(name).is_ok_and(|addr| std::os::unix::net::UnixStream::connect_addr(&addr).is_ok())
        || listening(&path)
}

/// 有 `WAYLAND_DISPLAY` 的是 Wayland，只有 `DISPLAY` 的是 X11。`DISPLAY` 只认本机的（`:0` 这样），ssh 转发
/// X 时设的 `localhost:10.0` 不是这台电脑的屏幕。
fn kind_of(env: &HashMap<String, String>) -> Option<Kind> {
    if env.get("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty()) {
        Some(Kind::Wayland)
    } else if env.get("DISPLAY").is_some_and(|value| value.starts_with(':')) {
        Some(Kind::X11)
    } else {
        None
    }
}

/// logind 的一个会话文件里要用的几项。
#[derive(Debug, Default, PartialEq, Eq)]
struct Record {
    id: String,
    uid: Option<u32>,
    kind: Option<Kind>,
    active: bool,
    /// 登录窗口（greeter）这类不算用户的桌面。
    user_class: bool,
    closing: bool,
    display: Option<String>,
}

fn parse_record(id: &str, text: &str) -> Record {
    let mut record = Record { id: id.to_owned(), ..Record::default() };
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        match key {
            "UID" => record.uid = value.parse().ok(),
            "TYPE" => {
                record.kind = match value {
                    "x11" => Some(Kind::X11),
                    "wayland" => Some(Kind::Wayland),
                    _ => None,
                }
            }
            "ACTIVE" => record.active = value == "1",
            "CLASS" => record.user_class = value == "user",
            "STATE" => record.closing = value == "closing",
            "DISPLAY" if !value.is_empty() => record.display = Some(value.to_owned()),
            _ => {}
        }
    }
    record
}

fn from_logind(dir: &Path, uid: u32) -> Option<Session> {
    let mut records: Vec<Record> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            // 目录里还有 `<会话>.ref` 这样的 FIFO，读它会一直等着；只读名字里没有点的普通文件。
            let id = entry.file_name().into_string().ok()?;
            if id.contains('.') || !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                return None;
            }
            let text = fs::read_to_string(entry.path()).ok()?;
            Some(parse_record(&id, &text))
        })
        .filter(|record| record.uid == Some(uid) && record.kind.is_some() && record.user_class && !record.closing)
        .collect();
    records.sort_by_key(|record| !record.active);
    let record = records.into_iter().next()?;
    let kind = record.kind?;
    let mut env = session_env(&record.id, uid).unwrap_or_default();
    env.entry("XDG_SESSION_ID".into()).or_insert_with(|| record.id.clone());
    if let Some(display) = &record.display {
        env.entry("DISPLAY".into()).or_insert_with(|| display.clone());
    }
    fill_defaults(&mut env, kind, uid);
    Some(Session { kind, env })
}

/// 会话 `id` 里本用户的进程交出的环境变量：取带 `WAYLAND_DISPLAY` 或 `DISPLAY`、带的变量最多的那个进程的。
fn session_env(id: &str, uid: u32) -> Option<HashMap<String, String>> {
    let mut best: Option<HashMap<String, String>> = None;
    for entry in fs::read_dir("/proc").ok()?.flatten() {
        let name = entry.file_name();
        if !name.as_encoded_bytes().iter().all(u8::is_ascii_digit) {
            continue;
        }
        let path = entry.path();
        if fs::metadata(&path).map(|meta| meta.uid()).ok() != Some(uid) {
            continue;
        }
        let Ok(bytes) = fs::read(path.join("environ")) else { continue };
        let env = parse_environ(&bytes);
        if env.get("XDG_SESSION_ID").map(String::as_str) != Some(id) || kind_of(&env).is_none() {
            continue;
        }
        if best.as_ref().is_none_or(|best| env.len() > best.len()) {
            best = Some(env);
        }
    }
    best
}

/// `/proc/<pid>/environ` 的内容（`NUL` 分隔的 `KEY=value`）里 `IMPORTED` 的那些。
fn parse_environ(bytes: &[u8]) -> HashMap<String, String> {
    bytes
        .split(|&byte| byte == 0)
        .filter_map(|pair| {
            let pair = std::str::from_utf8(pair).ok()?;
            let (key, value) = pair.split_once('=')?;
            IMPORTED.contains(&key).then(|| (key.to_owned(), value.to_owned()))
        })
        .collect()
}

/// 读不到会话里的进程时，按 systemd 的惯例补上运行目录和会话总线；Wayland 的 socket 取运行目录里的第一个。
fn fill_defaults(env: &mut HashMap<String, String>, kind: Kind, uid: u32) {
    let runtime = env.entry("XDG_RUNTIME_DIR".into()).or_insert_with(|| format!("/run/user/{uid}")).clone();
    let bus = Path::new(&runtime).join("bus");
    if bus.exists() {
        env.entry("DBUS_SESSION_BUS_ADDRESS".into()).or_insert_with(|| format!("unix:path={}", bus.display()));
    }
    if kind == Kind::Wayland
        && !env.contains_key("WAYLAND_DISPLAY")
        && let Some(socket) = wayland_sockets(Path::new(&runtime)).into_iter().next()
    {
        env.insert("WAYLAND_DISPLAY".into(), socket);
    }
    env.entry("XDG_SESSION_TYPE".into()).or_insert_with(|| match kind {
        Kind::X11 => "x11".into(),
        Kind::Wayland => "wayland".into(),
    });
}

/// 运行目录里的 Wayland socket 名（`wayland-0` 这类，不含锁文件），按名字排。
fn wayland_sockets(runtime: &Path) -> Vec<String> {
    let mut sockets: Vec<String> = fs::read_dir(runtime)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| name.starts_with("wayland-") && !name.ends_with(".lock"))
                .collect()
        })
        .unwrap_or_default();
    sockets.sort();
    sockets
}

/// 没有 logind 时：本用户运行目录里的 Wayland socket，或者本用户自己起的 X 服务器（socket 属于本用户）。服务器退出
/// 后 socket 文件可能还在，连得上的才算。
fn from_sockets(uid: u32) -> Option<Session> {
    let runtime = PathBuf::from(format!("/run/user/{uid}"));
    let mut env = HashMap::new();
    if let Some(socket) = wayland_sockets(&runtime).into_iter().find(|socket| listening(&runtime.join(socket))) {
        env.insert("WAYLAND_DISPLAY".to_owned(), socket);
        fill_defaults(&mut env, Kind::Wayland, uid);
        return Some(Session { kind: Kind::Wayland, env });
    }
    let display = fs::read_dir(X11_SOCKETS)
        .ok()?
        .flatten()
        .filter(|entry| entry.metadata().is_ok_and(|meta| meta.uid() == uid))
        .filter_map(|entry| {
            let name = OsString::from_vec(entry.file_name().into_encoded_bytes());
            name.to_str()?.strip_prefix('X')?.parse::<u32>().ok()
        })
        .filter(|&number| x_listening(number))
        .min()?;
    env.insert("DISPLAY".to_owned(), format!(":{display}"));
    fill_defaults(&mut env, Kind::X11, uid);
    Some(Session { kind: Kind::X11, env })
}

/// 有服务器在 Unix socket `path` 上监听。
fn listening(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_logind_session_records() {
        let text = "UID=1000\nUSER=me\nACTIVE=1\nSTATE=active\nTYPE=wayland\nCLASS=user\nDISPLAY=\n";
        let record = parse_record("3", text);
        assert_eq!(record.uid, Some(1000));
        assert_eq!(record.kind, Some(Kind::Wayland));
        assert!(record.active && record.user_class && !record.closing);
        assert_eq!(record.display, None);
        let greeter = parse_record("c1", "UID=120\nTYPE=x11\nCLASS=greeter\nDISPLAY=:0\n");
        assert!(!greeter.user_class);
        assert_eq!(greeter.display.as_deref(), Some(":0"));
        // ssh 的登录是 tty 会话，不算图形会话。
        assert_eq!(parse_record("5", "UID=1000\nTYPE=tty\nCLASS=user\n").kind, None);
    }

    #[test]
    fn picks_the_active_graphical_session_of_this_user() {
        let dir = std::env::temp_dir().join(format!("runode-sessions-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("2"), "UID=1000\nTYPE=tty\nCLASS=user\nACTIVE=1\n").unwrap();
        fs::write(dir.join("3"), "UID=1000\nTYPE=x11\nCLASS=user\nACTIVE=0\nDISPLAY=:1\n").unwrap();
        fs::write(dir.join("4"), "UID=1000\nTYPE=x11\nCLASS=user\nACTIVE=1\nDISPLAY=:0\n").unwrap();
        fs::write(dir.join("c1"), "UID=1001\nTYPE=wayland\nCLASS=user\nACTIVE=1\n").unwrap();
        // logind 在旁边放的 FIFO：读了会一直等着，要跳过。
        let fifo = std::ffi::CString::new(dir.join("4.ref").into_os_string().into_encoded_bytes()).unwrap();
        // SAFETY: 路径是以 NUL 结尾的字符串。
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0, "{}", std::io::Error::last_os_error());
        let session = from_logind(&dir, 1000).unwrap();
        assert_eq!(session.kind, Kind::X11);
        assert_eq!(session.env.get("DISPLAY").map(String::as_str), Some(":0"));
        assert_eq!(session.env.get("XDG_SESSION_ID").map(String::as_str), Some("4"));
        assert!(from_logind(&dir, 1002).is_none());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn inherited_variables_count_only_while_the_server_listens() {
        let dir = std::env::temp_dir().join(format!("runode-runtime-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let env = HashMap::from([
            ("XDG_RUNTIME_DIR".to_owned(), dir.display().to_string()),
            ("WAYLAND_DISPLAY".to_owned(), "wayland-9".to_owned()),
        ]);
        assert!(!server_running(&env, Kind::Wayland, 0));
        let listener = std::os::unix::net::UnixListener::bind(dir.join("wayland-9")).unwrap();
        assert!(server_running(&env, Kind::Wayland, 0));
        // 服务器退出后 socket 文件留着也不算。
        drop(listener);
        assert!(dir.join("wayland-9").exists());
        assert!(!server_running(&env, Kind::Wayland, 0));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn x_servers_listening_only_on_the_abstract_socket_count() {
        use std::os::{linux::net::SocketAddrExt as _, unix::net::SocketAddr};

        let number = 60_000 + std::process::id() % 5_000;
        let env = HashMap::from([("DISPLAY".to_owned(), format!(":{number}.0"))]);
        assert!(!server_running(&env, Kind::X11, 0));
        let name = format!("{X11_SOCKETS}/X{number}");
        let addr = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let _listener = std::os::unix::net::UnixListener::bind_addr(&addr).unwrap();
        assert!(server_running(&env, Kind::X11, 0));
        assert_eq!(x_display_number(":1"), Some(1));
        assert_eq!(x_display_number("localhost:10.0"), None);
    }

    #[test]
    fn keeps_only_the_imported_variables() {
        let env = parse_environ(b"HOME=/home/me\0DISPLAY=:0\0WAYLAND_DISPLAY=wayland-0\0XDG_SESSION_ID=4\0");
        assert_eq!(env.len(), 3);
        assert_eq!(kind_of(&env), Some(Kind::Wayland));
    }
}
