//! 尺寸归属：几个前端带着屏幕看同一个会话时，最近交互过的那个（owner）的视图决定会话的尺寸。

mod common;

use std::time::{Duration, Instant};

use common::{BUILD, Peer, WAIT, host, script, temp_dir};
use runode_host::{ClientMsg, Host, HostMsg, SessionId};
use runode_protocol::{AttachMode, BuildId, Caps, ClientKind, FrameKind, PROTOCOL_VERSION, SessionInfo};
use runode_shared_types::grid::GridSize;

const SIZE_A: GridSize = GridSize { cols: 30, rows: 6, cell_width_px: 8, cell_height_px: 16 };
const SIZE_B: GridSize = GridSize { cols: 40, rows: 8, cell_width_px: 8, cell_height_px: 16 };
const SIZE_C: GridSize = GridSize { cols: 50, rows: 10, cell_width_px: 8, cell_height_px: 16 };

/// 经 `Host::connect_pair` 连上，按 `client` 握手，报设备名 `device`。
fn connect(host: &Host, client: ClientKind, device: Option<&str>) -> Peer {
    let mut peer = Peer::over(host.connect_pair().unwrap());
    peer.send(&ClientMsg::Hello {
        protocol: PROTOCOL_VERSION,
        build: BuildId(BUILD.into()),
        client,
        caps: Caps { snapshot: true, vt_replay: true },
        session: None,
        device: device.map(Into::into),
    });
    assert!(matches!(peer.message(), HostMsg::Welcome { .. }));
    peer
}

fn desktop(host: &Host, device: &str) -> Peer {
    connect(host, ClientKind::Desktop, Some(device))
}

/// 连接上收到的一件事：某个通道的输出，或者一条控制消息。
#[derive(Debug)]
enum Seen {
    Output(u32, Vec<u8>),
    Msg(Box<HostMsg>),
}

/// 一直读，直到 `done` 认出一条控制消息，返回在那之前（含）收到的输出和控制消息。快照帧不要。
fn until(peer: &Peer, mut done: impl FnMut(&HostMsg) -> bool) -> Vec<Seen> {
    let deadline = Instant::now() + WAIT;
    let mut seen = Vec::new();
    loop {
        let frame = peer
            .frames
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_else(|_| panic!("timed out; saw {} things", seen.len()));
        match frame.kind {
            FrameKind::Output => seen.push(Seen::Output(frame.channel, frame.payload)),
            FrameKind::Control => {
                let message: HostMsg = frame.message().unwrap();
                let finished = done(&message);
                seen.push(Seen::Msg(Box::new(message)));
                if finished {
                    return seen;
                }
            }
            _ => {}
        }
    }
}

fn messages(seen: &[Seen]) -> impl Iterator<Item = &HostMsg> {
    seen.iter().filter_map(|seen| match seen {
        Seen::Msg(message) => Some(&**message),
        Seen::Output(..) => None,
    })
}

/// 收到的 `SizeOwner`（`mine`、`owner`），按先后。
fn owners(seen: &[Seen]) -> Vec<(bool, Option<String>)> {
    messages(seen)
        .filter_map(|message| match message {
            HostMsg::SizeOwner { mine, owner, .. } => Some((*mine, owner.clone())),
            _ => None,
        })
        .collect()
}

/// 收到的 `Resized` 的尺寸，按先后。
fn resized(seen: &[Seen]) -> Vec<GridSize> {
    messages(seen)
        .filter_map(|message| match message {
            HostMsg::Resized { size, .. } => Some(*size),
            _ => None,
        })
        .collect()
}

fn size_owner(mine: bool, owner: &str) -> (bool, Option<String>) {
    (mine, Some(owner.into()))
}

/// 连上会话 `id`，读到 `SnapshotEnd`，返回通道、`Attached` 给的尺寸和在那之前收到的东西（同一条
/// 连接原来连着时，旧通道的事件排在新的 `Attached` 前面）。
fn attach(peer: &mut Peer, id: SessionId, size: Option<GridSize>, mode: AttachMode) -> (u32, GridSize, Vec<Seen>) {
    peer.send(&ClientMsg::Attach { id, size, mode });
    let before = until(peer, |message| matches!(message, HostMsg::Attached { .. }));
    let Some(HostMsg::Attached { channel, size, mode: given, .. }) = messages(&before).last() else {
        unreachable!();
    };
    let (channel, size, given) = (*channel, *size, *given);
    peer.screen(id, channel, given);
    (channel, size, before)
}

/// 带着屏幕连上会话，`Attached` 之后（`SnapshotEnd` 后面）紧跟着的控制消息应当是 `SizeOwner`，
/// 返回通道、`Attached` 给的尺寸和那条 `SizeOwner`。
fn attach_view(peer: &mut Peer, id: SessionId, size: Option<GridSize>) -> (u32, GridSize, (bool, Option<String>)) {
    let (channel, size, _) = attach(peer, id, size, AttachMode::VtReplay);
    let after = loop {
        match peer.message() {
            HostMsg::Meta { .. } => {}
            message => break message,
        }
    };
    let HostMsg::SizeOwner { id: owned, mine, owner } = after else {
        panic!("expected the size owner right after attaching, got {after:?}");
    };
    assert_eq!(owned, id);
    (channel, size, (mine, owner))
}

/// 列会话时 `id` 那一项，以及在回话之前这条连接上收到的东西：列会话的请求排在这条连接之前
/// 送到会话线程的请求后面，回话之前收到的就是那些请求引起的全部事件。
fn info(peer: &mut Peer, id: SessionId) -> (SessionInfo, Vec<Seen>) {
    peer.send(&ClientMsg::ListSessions);
    let seen = until(peer, |message| matches!(message, HostMsg::SessionList { .. }));
    let Some(HostMsg::SessionList { sessions }) = messages(&seen).last() else { unreachable!() };
    let info = sessions.iter().find(|info| info.id == id).expect("the session is listed").clone();
    (info, seen)
}

/// 等这条连接之前的请求都在会话线程里办完：读一次屏幕，之前收到的都不要。
fn settle(peer: &mut Peer, id: SessionId) {
    peer.send(&ClientMsg::ReadScreen { id, lines: None, command: None });
    until(peer, |message| matches!(message, HostMsg::ScreenText { .. }));
}

fn cat(peer: &mut Peer) -> SessionId {
    peer.spawn("/bin/cat")
}

/// 两个前端带着尺寸连上：后连上的说了算。先连上的在输出流里收到 `Resized` 和不是自己的
/// `SizeOwner`，后连上的拿到按自己尺寸的屏幕，`Attached` 之后收到是自己的 `SizeOwner`。
#[test]
fn the_last_to_attach_with_a_size_decides() {
    let host = host();
    let mut a = desktop(&host, "alpha");
    let mut b = desktop(&host, "beta");
    let id = cat(&mut a);
    let (_, size, owner) = attach_view(&mut a, id, Some(SIZE_A));
    assert_eq!((size, owner), (SIZE_A, size_owner(true, "alpha")));
    let (_, size, owner) = attach_view(&mut b, id, Some(SIZE_B));
    assert_eq!((size, owner), (SIZE_B, size_owner(true, "beta")));
    let seen = until(&a, |message| matches!(message, HostMsg::SizeOwner { .. }));
    assert_eq!(resized(&seen), [SIZE_B]);
    assert_eq!(owners(&seen), [size_owner(false, "beta")]);
    let (info, _) = info(&mut a, id);
    assert_eq!((info.size, info.size_owner.as_deref()), (SIZE_B, Some("beta")));
    a.send(&ClientMsg::Kill { id });
}

/// 不是 owner 的前端打字就接管：`Resized` 在两条连接的输出流里落在同一个位置（前面的输出一样），
/// 两边各自收到新的 `SizeOwner`。
#[test]
fn typing_takes_the_size_at_the_same_point_in_both_streams() {
    let dir = temp_dir("sizing-typing");
    let ticking = script(&dir, "ticking", "i=0\nwhile :; do i=$((i+1)); echo tick$i; sleep 0.01; done");
    let host = host();
    let mut a = desktop(&host, "alpha");
    let mut b = desktop(&host, "beta");
    let id = a.spawn(&ticking);
    let (channel_a, ..) = attach_view(&mut a, id, Some(SIZE_A));
    // 等程序跑起来，`Resized` 前后都有输出。
    a.wait_for_output(channel_a, b"tick2");
    let (channel_b, ..) = attach_view(&mut b, id, Some(SIZE_B));
    // a 这边：b 接管之后的事件和 b 在 `Attached` 之后收到的一样。
    until(&a, |message| matches!(message, HostMsg::SizeOwner { mine: false, .. }));
    std::thread::sleep(Duration::from_millis(100));
    a.input(channel_a, b"x");
    let is_resized_to_a = |message: &HostMsg| matches!(message, HostMsg::Resized { size, .. } if *size == SIZE_A);
    let seen_a = until(&a, is_resized_to_a);
    let seen_b = until(&b, is_resized_to_a);
    let output = |seen: &[Seen], channel: u32| -> Vec<u8> {
        seen.iter()
            .filter_map(|seen| match seen {
                Seen::Output(on, data) if *on == channel => Some(data.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    };
    let (before_a, before_b) = (output(&seen_a, channel_a), output(&seen_b, channel_b));
    assert!(common::contains(&before_a, b"tick"), "no output before the resize: {seen_a:?}");
    assert_eq!(String::from_utf8_lossy(&before_a), String::from_utf8_lossy(&before_b));
    assert!(resized(&seen_a[..seen_a.len() - 1]).is_empty());
    assert!(resized(&seen_b[..seen_b.len() - 1]).is_empty());
    let next_owner = |peer: &Peer| loop {
        if let HostMsg::SizeOwner { mine, owner, .. } = peer.message() {
            return (mine, owner);
        }
    };
    assert_eq!(next_owner(&a), size_owner(true, "alpha"));
    assert_eq!(next_owner(&b), size_owner(false, "alpha"));
    a.send(&ClientMsg::Kill { id });
}

/// owner 断开（`Detach` 或者连接断了）时交给剩下的里面最近交互过的那个（打字和获得焦点都算），
/// 应用它记着的尺寸。
#[test]
fn the_most_recent_viewer_takes_over_when_the_owner_leaves() {
    let host = host();
    let mut a = desktop(&host, "alpha");
    let mut b = desktop(&host, "beta");
    let mut c = desktop(&host, "gamma");
    let id = cat(&mut a);
    let (channel_a, ..) = attach_view(&mut a, id, Some(SIZE_A));
    attach_view(&mut b, id, Some(SIZE_B));
    attach_view(&mut c, id, Some(SIZE_C));
    a.input(channel_a, b"x");
    settle(&mut a, id);
    b.send(&ClientMsg::Focus { id, focused: true });
    settle(&mut b, id);
    let (info_now, _) = info(&mut c, id);
    assert_eq!((info_now.size, info_now.size_owner.as_deref()), (SIZE_B, Some("beta")));
    // 读掉 a 到这里为止收到的。
    info(&mut a, id);

    // b 走了：a 比 c 交互得晚。
    b.send(&ClientMsg::Detach { id });
    let seen = until(&a, |message| matches!(message, HostMsg::SizeOwner { mine: true, .. }));
    assert_eq!(resized(&seen).last(), Some(&SIZE_A));
    let (info_now, _) = info(&mut c, id);
    assert_eq!((info_now.size, info_now.size_owner.as_deref()), (SIZE_A, Some("alpha")));

    // a 的连接断了：只剩 c。
    drop(a);
    let seen = until(&c, |message| matches!(message, HostMsg::SizeOwner { mine: true, .. }));
    assert_eq!(resized(&seen).last(), Some(&SIZE_C));
    assert_eq!(owners(&seen).last(), Some(&size_owner(true, "gamma")));
    let (info_now, _) = info(&mut c, id);
    assert_eq!((info_now.size, info_now.size_owner.as_deref()), (SIZE_C, Some("gamma")));
    c.send(&ClientMsg::Kill { id });
}

/// 只看状态的连接和命令行没有资格：它们连上（带着尺寸也一样）、发输入帧、发键、粘贴、改尺寸、
/// 报焦点都不改 owner 和尺寸。改成只看状态的 owner 让出尺寸。
#[test]
fn meta_only_and_command_line_input_do_not_take_the_size() {
    let host = host();
    let mut a = desktop(&host, "alpha");
    let mut cli = connect(&host, ClientKind::Cli, Some("shell"));
    let id = cat(&mut a);
    attach_view(&mut a, id, Some(SIZE_A));
    let (channel, ..) = attach(&mut cli, id, Some(SIZE_B), AttachMode::MetaOnly);
    cli.input(channel, b"typed\r");
    cli.send(&ClientMsg::SendKeys { req: 7, id, keys: vec!["enter".into()] });
    assert!(matches!(cli.reply(), HostMsg::Done { req: 7 }));
    cli.send(&ClientMsg::Paste { req: 8, id, text: "pasted".into() });
    assert!(matches!(cli.reply(), HostMsg::Done { req: 8 }));
    cli.send(&ClientMsg::Resize { id, size: SIZE_C });
    cli.send(&ClientMsg::Focus { id, focused: true });
    settle(&mut cli, id);
    let (info_now, seen) = info(&mut a, id);
    assert_eq!((info_now.size, info_now.size_owner.as_deref()), (SIZE_A, Some("alpha")));
    assert!(resized(&seen).is_empty(), "{seen:?}");
    assert!(owners(&seen).is_empty(), "{seen:?}");

    // 一个桌面把这个会话的标签放到后台（改成只看状态）：不再有资格，也没有别人，没有 owner 了，
    // 谁的尺寸都照旧应用。
    let mut b = desktop(&host, "beta");
    attach_view(&mut b, id, Some(SIZE_B));
    attach(&mut b, id, None, AttachMode::MetaOnly);
    let seen = until(&a, |message| matches!(message, HostMsg::SizeOwner { mine: true, .. }));
    assert_eq!(resized(&seen).last(), Some(&SIZE_A));
    a.send(&ClientMsg::Detach { id });
    settle(&mut a, id);
    let (info_now, _) = info(&mut cli, id);
    assert_eq!(info_now.size_owner, None);
    cli.send(&ClientMsg::Resize { id, size: SIZE_C });
    settle(&mut cli, id);
    assert_eq!(info(&mut cli, id).0.size, SIZE_C);
    cli.send(&ClientMsg::Kill { id });
}

/// 还没有 owner 时（前端带着屏幕连上但没带尺寸、也还没交互过）谁的 `Resize` 都照旧应用，不发
/// `SizeOwner`；交互一次就有了 owner。
#[test]
fn without_an_owner_every_resize_applies() {
    let host = host();
    let mut a = desktop(&host, "alpha");
    let mut b = desktop(&host, "beta");
    let id = cat(&mut a);
    attach(&mut a, id, None, AttachMode::Snapshot);
    attach(&mut b, id, None, AttachMode::Snapshot);
    a.send(&ClientMsg::Resize { id, size: SIZE_A });
    let seen = until(&b, |message| matches!(message, HostMsg::Resized { .. }));
    assert_eq!(resized(&seen), [SIZE_A]);
    b.send(&ClientMsg::Resize { id, size: SIZE_B });
    let seen = until(&a, |message| matches!(message, HostMsg::Resized { size, .. } if *size == SIZE_B));
    assert!(owners(&seen).is_empty(), "{seen:?}");
    let (info_now, seen) = info(&mut a, id);
    assert_eq!((info_now.size, info_now.size_owner), (SIZE_B, None));
    assert!(owners(&seen).is_empty(), "{seen:?}");

    // a 获得焦点：轮到它，应用它最近请求的尺寸。
    a.send(&ClientMsg::Focus { id, focused: true });
    let seen = until(&b, |message| matches!(message, HostMsg::SizeOwner { .. }));
    assert_eq!(resized(&seen).last(), Some(&SIZE_A));
    assert_eq!(owners(&seen), [size_owner(false, "alpha")]);
    a.send(&ClientMsg::Kill { id });
}

/// 失去焦点不算交互，也不让出尺寸。
#[test]
fn losing_focus_keeps_the_size() {
    let host = host();
    let mut a = desktop(&host, "alpha");
    let mut b = desktop(&host, "beta");
    let id = cat(&mut a);
    attach_view(&mut a, id, Some(SIZE_A));
    attach_view(&mut b, id, Some(SIZE_B));
    b.send(&ClientMsg::Focus { id, focused: false });
    a.send(&ClientMsg::Focus { id, focused: false });
    settle(&mut b, id);
    settle(&mut a, id);
    let (info_now, seen) = info(&mut b, id);
    assert_eq!((info_now.size, info_now.size_owner.as_deref()), (SIZE_B, Some("beta")));
    assert!(owners(&seen).is_empty(), "{seen:?}");
    assert!(resized(&seen).is_empty(), "{seen:?}");
    a.send(&ClientMsg::Kill { id });
}

/// 不是 owner 的视图改尺寸只记下来；它带着屏幕新连上（不带尺寸）时在 `Attached` 之后收到当前的
/// `SizeOwner`；重新连上（`Resync` 后那样不带尺寸）不抢也不丢 owner；当上 owner 时应用它记着的尺寸。
#[test]
fn a_viewer_without_the_size_waits_its_turn() {
    let host = host();
    let mut a = desktop(&host, "alpha");
    let mut b = desktop(&host, "beta");
    let id = cat(&mut a);
    attach_view(&mut a, id, Some(SIZE_A));
    let (_, size, owner) = attach_view(&mut b, id, None);
    assert_eq!((size, owner), (SIZE_A, size_owner(false, "alpha")));
    b.send(&ClientMsg::Resize { id, size: SIZE_B });
    settle(&mut b, id);
    let (info_now, seen) = info(&mut a, id);
    assert_eq!((info_now.size, info_now.size_owner.as_deref()), (SIZE_A, Some("alpha")));
    assert!(resized(&seen).is_empty(), "{seen:?}");

    // a 重新连上：还是 owner。
    let (_, size, owner) = attach_view(&mut a, id, None);
    assert_eq!((size, owner), (SIZE_A, size_owner(true, "alpha")));
    // b 重新连上：还不是。
    let (_, size, owner) = attach_view(&mut b, id, None);
    assert_eq!((size, owner), (SIZE_A, size_owner(false, "alpha")));

    // b 接管，用它之前请求的尺寸；重新连上后它还是 owner。
    b.send(&ClientMsg::Focus { id, focused: true });
    let seen = until(&a, |message| matches!(message, HostMsg::SizeOwner { .. }));
    assert_eq!(resized(&seen), [SIZE_B]);
    assert_eq!(owners(&seen), [size_owner(false, "beta")]);
    let (_, size, owner) = attach_view(&mut b, id, None);
    assert_eq!((size, owner), (SIZE_B, size_owner(true, "beta")));
    assert_eq!(info(&mut a, id).0.size_owner.as_deref(), Some("beta"));
    a.send(&ClientMsg::Kill { id });
}

/// 没报设备名的前端当 owner 时，`SizeOwner` 和列会话的 `size_owner` 都是空的；它照样决定尺寸。
#[test]
fn an_owner_without_a_device_name() {
    let host = host();
    let mut a = desktop(&host, "alpha");
    let mut anonymous = connect(&host, ClientKind::Desktop, None);
    let id = cat(&mut a);
    attach_view(&mut a, id, Some(SIZE_A));
    let (_, size, owner) = attach_view(&mut anonymous, id, Some(SIZE_B));
    assert_eq!((size, owner), (SIZE_B, (true, None)));
    let seen = until(&a, |message| matches!(message, HostMsg::SizeOwner { .. }));
    assert_eq!(owners(&seen), [(false, None)]);
    let (info_now, _) = info(&mut a, id);
    assert_eq!((info_now.size, info_now.size_owner), (SIZE_B, None));
    a.send(&ClientMsg::Kill { id });
}

fn mobile(host: &Host, device: &str) -> Peer {
    connect(host, ClientKind::Mobile, Some(device))
}

/// 带着屏幕连上却从没请求过尺寸的连接（手机跟着 Mac 的尺寸看）没有资格：它打字、获得焦点都不算
/// 交互，抢不走 owner，也不引起 `SizeOwner`、`Resized`。没有 owner 时它打字也不会当上。
#[test]
fn a_viewer_that_never_asked_for_a_size_does_not_take_it() {
    let host = host();
    let mut a = desktop(&host, "alpha");
    let mut phone = mobile(&host, "phone");
    let id = cat(&mut a);
    let alone = cat(&mut a);
    attach_view(&mut a, id, Some(SIZE_A));
    let (channel, size, owner) = attach_view(&mut phone, id, None);
    assert_eq!((size, owner), (SIZE_A, size_owner(false, "alpha")));
    phone.input(channel, b"typed\r");
    phone.send(&ClientMsg::Focus { id, focused: true });
    settle(&mut phone, id);
    let (info_now, seen) = info(&mut a, id);
    assert_eq!((info_now.size, info_now.size_owner.as_deref()), (SIZE_A, Some("alpha")));
    assert!(owners(&seen).is_empty(), "{seen:?}");
    assert!(resized(&seen).is_empty(), "{seen:?}");
    let (_, seen) = info(&mut phone, id);
    assert!(owners(&seen).is_empty(), "{seen:?}");
    a.send(&ClientMsg::Kill { id });

    // 只有手机看着的会话：打字、获得焦点以后照样没有 owner。
    let id = alone;
    let (channel, ..) = attach(&mut phone, id, None, AttachMode::VtReplay);
    phone.input(channel, b"typed\r");
    phone.send(&ClientMsg::Focus { id, focused: true });
    let (info_now, seen) = info(&mut phone, id);
    assert_eq!(info_now.size_owner, None);
    assert!(owners(&seen).is_empty(), "{seen:?}");
    a.send(&ClientMsg::Kill { id });
}

/// owner 走了，只交给请求过尺寸的：剩下的里有就交给最近交互过的那个，只剩没请求过尺寸的就没有
/// owner，不发 `SizeOwner`。
#[test]
fn the_owner_leaving_skips_viewers_without_a_size() {
    let host = host();
    let mut a = desktop(&host, "alpha");
    let mut b = desktop(&host, "beta");
    let mut phone = mobile(&host, "phone");
    let id = cat(&mut a);
    attach_view(&mut b, id, Some(SIZE_B));
    let (channel_a, ..) = attach_view(&mut a, id, Some(SIZE_A));
    let (channel_phone, ..) = attach_view(&mut phone, id, None);
    a.input(channel_a, b"x");
    settle(&mut a, id);
    // 手机最后打的字，也不算。
    phone.input(channel_phone, b"y");
    settle(&mut phone, id);
    info(&mut b, id);

    a.send(&ClientMsg::Detach { id });
    let seen = until(&b, |message| matches!(message, HostMsg::SizeOwner { .. }));
    assert_eq!(owners(&seen), [size_owner(true, "beta")]);
    assert_eq!(resized(&seen).last(), Some(&SIZE_B));
    let (info_now, seen) = info(&mut phone, id);
    assert_eq!((info_now.size, info_now.size_owner.as_deref()), (SIZE_B, Some("beta")));
    assert_eq!(owners(&seen), [size_owner(false, "beta")]);

    // b 也走了：只剩手机，没有 owner。
    drop(b);
    settle(&mut phone, id);
    let (info_now, seen) = info(&mut phone, id);
    assert_eq!((info_now.size, info_now.size_owner), (SIZE_B, None));
    assert!(owners(&seen).is_empty(), "{seen:?}");
    assert!(resized(&seen).is_empty(), "{seen:?}");
    a.send(&ClientMsg::Kill { id });
}
