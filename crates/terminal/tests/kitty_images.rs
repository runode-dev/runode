//! 程序用 Kitty 图形协议放到屏幕上的图片：界面这份 VT 交进帧里，宿主那份应答查询、带进快照，
//! 按文件传的由宿主读出来改成直接传。

mod common;

use std::{path::PathBuf, time::Duration};

use common::{file_command, idle_host, idle_session};
use runode_terminal::session::Session;
use runode_types::frame::ImageLayer;

/// 2×2 像素的 RGBA：红、绿、蓝、白。
const PIXELS: &[u8] = &[255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255];

/// 把 `PIXELS` 当 1 号图片传过去，铺在光标处 2 列 1 行。
const PLACE: &[u8] = b"\x1b_Ga=T,f=32,i=1,s=2,v=2,c=2,r=1,q=2;/wAA/wD/AP8AAP///////w==\x1b\\";

#[test]
fn a_placed_image_is_in_the_frame() {
    let mut session = idle_session();
    session.feed(b"ab");
    session.feed(PLACE);
    let [image] = session.frame().images.clone().try_into().unwrap();
    assert_eq!((image.x, image.y, image.width, image.height), (2., 0., 2., 1.));
    assert_eq!(image.source, [0, 0, 2, 2]);
    assert_eq!(image.layer, ImageLayer::AboveText);
    assert_eq!(session.image_rgba(image.id, image.generation), Some((2, 2, PIXELS.to_vec())));
}

/// 图片跟着文字滚：滚出视口一半时左上角在视口上面，删掉后帧里就没了。
#[test]
fn images_scroll_with_the_text_and_go_away_when_deleted() {
    let mut session = idle_session();
    session.feed(b"\x1b_Ga=T,f=32,i=1,s=2,v=2,c=2,r=2,q=2;/wAA/wD/AP8AAP///////w==\x1b\\");
    session.feed(b"\r\n\r\n\r\n");
    assert_eq!(session.frame().images.first().map(|image| image.y), Some(-1.));
    session.feed(b"\x1b_Ga=d,d=A,q=2\x1b\\");
    assert!(session.frame().images.is_empty());
}

/// 全屏程序（yazi 一类）在备用屏上放的图片也要画。
#[test]
fn images_on_the_alternate_screen_are_in_the_frame() {
    let mut session = idle_session();
    session.feed(b"\x1b[?1049h");
    session.feed(PLACE);
    assert_eq!(session.frame().images.len(), 1);
}

/// 虚拟放置的图片画在占位符的格子上，占位符本身不画；图片没加载上时那几格也是空白。
#[test]
fn unicode_placeholders_are_blank() {
    // 前景色 1 指 1 号图片，U+0305、U+030D 分别记第 0、1 号，两格是第 0 行的第 0、1 列。
    let placeholders = "\x1b[38;5;1m\u{10EEEE}\u{0305}\u{0305}\u{10EEEE}\u{0305}\u{030D}\x1b[m|";
    let mut session = idle_session();
    session.feed(placeholders.as_bytes());
    assert_eq!(common::row_text(&session.frame(), 0), "  |");

    let mut session = idle_session();
    session.feed(b"\x1b_Ga=T,U=1,f=32,i=1,s=2,v=2,c=2,r=1,q=2;/wAA/wD/AP8AAP///////w==\x1b\\");
    session.feed(placeholders.as_bytes());
    let frame = session.frame();
    assert_eq!(common::row_text(&frame, 0), "  |");
    let [image] = frame.images.clone().try_into().unwrap();
    assert_eq!((image.x, image.y, image.width, image.height), (0., 0., 2., 1.));
}

/// 界面那份 VT 自己不读文件：按文件传的图要宿主改写成直接传才收得到。
#[test]
fn images_read_from_files_are_refused() {
    let path = temp_file("refused", PIXELS);
    let mut session = idle_session();
    session.feed(&file_command("a=T,t=f,f=32,i=1,s=2,v=2,q=2", &path));
    std::fs::remove_file(&path).unwrap();
    assert!(session.frame().images.is_empty());
}

#[test]
fn the_host_answers_image_queries() {
    let mut host = idle_host();
    let replies = host.replies();
    host.feed(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\");
    assert_eq!(host.replies(), replies + 1);
}

/// 换宿主接手、界面重连时图片跟着快照过去，不用程序重发。
#[test]
fn images_survive_a_snapshot() {
    let mut host = idle_host();
    host.feed(PLACE);
    let mut session = Session::from_snapshot(&host.snapshot().unwrap(), Box::new(|_| {})).unwrap();
    assert_eq!(session.frame().images.len(), 1);
}

/// 有动画在播时帧里带着下一帧什么时候该画，停了就没有。
#[test]
fn a_playing_animation_schedules_the_next_frame() {
    let mut session = idle_session();
    session.feed(
        b"\x1b_Ga=T,f=32,i=1,s=1,v=1,q=2;/wAA/w==\x1b\\\
          \x1b_Ga=f,f=32,i=1,s=1,v=1,z=60,q=2;AAD//w==\x1b\\\
          \x1b_Ga=a,i=1,r=1,z=25,q=2\x1b\\\
          \x1b_Ga=a,i=1,s=3,q=2\x1b\\",
    );
    let next = session.frame().next_image_frame;
    assert!(next.is_some_and(|next| next <= Duration::from_millis(25)), "{next:?}");
    session.feed(b"\x1b_Ga=a,i=1,s=1,q=2\x1b\\");
    assert_eq!(session.frame().next_image_frame, None);
}

/// 在临时目录里写一个这个测试进程专用的文件，名字里带着 `name`。
fn temp_file(name: &str, contents: &[u8]) -> PathBuf {
    let path = std::env::temp_dir().join(format!("runode-kitty-{}-{name}.rgba", std::process::id()));
    std::fs::write(&path, contents).unwrap();
    path
}

/// 宿主把按文件、临时文件传的图改写成直接传，只在 macOS 和 Linux 上做。
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod files {
    use runode_terminal::{
        SnapshotError,
        host_session::{HostSession, ReportRedactor},
    };

    use super::*;
    use crate::common::base64;

    /// 把 `PIXELS` 按文件传、当 1 号图片铺在光标处 2 列 1 行的控制键。
    const PLACE_KEYS: &str = "a=T,t=f,f=32,i=1,s=2,v=2,c=2,r=1,q=2";

    /// 转出去的字节里没有路径、有像素，界面那份喂它就有图，宿主自己的快照里也有。
    #[test]
    fn the_host_sends_images_read_from_files_inline() {
        let path = temp_file("inline", PIXELS);
        let mut host = idle_host();
        let fed = host.feed_output(&file_command(PLACE_KEYS, &path));
        std::fs::remove_file(&path).unwrap();
        let forwarded = fed.rewritten.expect("the command is rewritten");
        assert!(!contains(&forwarded, base64(path.to_str().unwrap().as_bytes()).as_bytes()));
        assert!(contains(&forwarded, base64(PIXELS).as_bytes()));

        let mut session = idle_session();
        session.feed(&forwarded);
        let [image] = session.frame().images.clone().try_into().unwrap();
        assert_eq!((image.x, image.y, image.width, image.height), (0., 0., 2., 1.));
        assert_eq!(session.image_rgba(image.id, image.generation), Some((2, 2, PIXELS.to_vec())));

        let mut restored = Session::from_snapshot(&host.snapshot().unwrap(), Box::new(|_| {})).unwrap();
        assert_eq!(restored.frame().images.len(), 1);
    }

    /// 命令切在两块输出之间，从哪个字节切开转出去的都一样。
    #[test]
    fn a_file_command_cut_anywhere_is_rewritten_the_same() {
        let path = temp_file("cut", PIXELS);
        let command = [&b"ab"[..], &file_command(PLACE_KEYS, &path), b"cd"].concat();
        let mut host = idle_host();
        let whole = forwarded(&mut host, &command);
        assert!(contains(&whole, base64(PIXELS).as_bytes()));
        for cut in 0..=command.len() {
            let (head, tail) = command.split_at(cut);
            let mut out = forwarded(&mut host, head);
            out.extend(forwarded(&mut host, tail));
            assert_eq!(out, whole, "cut at {cut}");
        }
        std::fs::remove_file(&path).unwrap();
    }

    /// 临时文件（`t=t`）读完就删。
    #[test]
    fn temporary_files_are_deleted_once_read() {
        let path = temp_file("tty-graphics-protocol", PIXELS);
        let mut host = idle_host();
        let fed = host.feed_output(&file_command("a=T,t=t,f=32,i=1,s=2,v=2,q=2", &path));
        assert!(!path.exists());
        assert!(contains(&fed.rewritten.unwrap(), base64(PIXELS).as_bytes()));
    }

    /// 读不成的原样交给 VT，它回不支持（`q=1` 只回错误）；读成了的 `q=1` 不回话。
    #[test]
    fn unreadable_files_pass_through_and_get_an_error() {
        let missing = std::env::temp_dir().join(format!("runode-kitty-{}-missing.rgba", std::process::id()));
        let command = file_command("a=T,t=f,f=32,i=1,s=2,v=2,q=1", &missing);
        let mut host = idle_host();
        let replies = host.replies();
        let fed = host.feed_output(&command);
        assert!(fed.rewritten.is_none_or(|bytes| bytes == command));
        assert_eq!(host.replies(), replies + 1);

        let path = temp_file("quiet", PIXELS);
        let replies = host.replies();
        host.feed_output(&file_command("a=T,t=f,f=32,i=2,s=2,v=2,q=1", &path));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(host.replies(), replies);
    }

    /// 按文件问支不支持（`a=q`）时宿主读出来改成直接传再问，只回一次。
    #[test]
    fn a_query_by_file_is_answered_once() {
        let path = temp_file("query", &[0, 0, 0]);
        let mut host = idle_host();
        let replies = host.replies();
        host.feed_output(&file_command("a=q,t=f,i=31,s=1,v=1,f=24,q=0", &path));
        std::fs::remove_file(&path).unwrap();
        assert_eq!(host.replies(), replies + 1);
    }

    /// 正扣着一条命令时原始快照编不出来（扣着的字节会丢）；给前端的快照和重放编得出，接着喂之后
    /// 转来的字节就有图。
    #[test]
    fn a_screen_given_while_a_command_is_held_resumes() {
        let path = temp_file("held", PIXELS);
        let command = file_command(PLACE_KEYS, &path);
        let (head, tail) = command.split_at(command.len() - 10);
        let mut host = idle_host();
        assert_eq!(forwarded(&mut host, head), b"\x1b_");
        assert!(matches!(host.snapshot(), Err(SnapshotError::Unfinished)));

        let redactor = ReportRedactor::new();
        let snapshot = host.redacted_snapshot(&redactor).unwrap();
        let mut view = Session::from_snapshot(&snapshot, Box::new(|_| {})).unwrap();
        let mut replayed = idle_session();
        replayed.feed(&host.redacted_vt_replay(&redactor).unwrap());

        let rest = forwarded(&mut host, tail);
        std::fs::remove_file(&path).unwrap();
        for session in [&mut view, &mut replayed] {
            session.feed(&rest);
            let [image] = session.frame().images.clone().try_into().unwrap();
            assert_eq!(session.image_rgba(image.id, image.generation), Some((2, 2, PIXELS.to_vec())));
        }
        assert!(host.snapshot().is_ok());
    }

    /// 比一块大的图改写成几块直接传，界面那份 VT 收下的是完整的图。
    #[test]
    fn a_large_image_arrives_whole_in_several_chunks() {
        let (width, height) = (64, 64);
        let pixels: Vec<u8> = (0..width * height * 4).map(|n| (n * 7 % 251) as u8).collect();
        let path = temp_file("several-chunks", &pixels);
        let keys = format!("a=T,t=f,f=32,i=1,s={width},v={height},c=4,r=2,q=2");
        let mut host = idle_host();
        let fed = host.feed_output(&file_command(&keys, &path));
        std::fs::remove_file(&path).unwrap();
        let forwarded = fed.rewritten.expect("the command is rewritten");
        assert!(forwarded.windows(3).filter(|w| w == b"\x1b_G").count() > 1, "sent in one chunk");

        let mut session = idle_session();
        session.feed(&forwarded);
        let [image] = session.frame().images.clone().try_into().unwrap();
        assert_eq!(session.image_rgba(image.id, image.generation), Some((width, height, pixels)));
    }

    /// 压缩过的文件（`o=z`）原样转过去，由 VT 解压。
    #[test]
    fn compressed_files_are_inflated() {
        let path = temp_file("zlib", &zlib_stored(PIXELS));
        let mut host = idle_host();
        let fed = host.feed_output(&file_command("a=T,t=f,o=z,f=32,i=1,s=2,v=2,q=2", &path));
        std::fs::remove_file(&path).unwrap();
        let mut session = idle_session();
        session.feed(&fed.rewritten.unwrap());
        let [image] = session.frame().images.clone().try_into().unwrap();
        assert_eq!(session.image_rgba(image.id, image.generation), Some((2, 2, PIXELS.to_vec())));
    }

    /// 宿主喂了 `data` 以后转出去的字节。
    fn forwarded(host: &mut HostSession, data: &[u8]) -> Vec<u8> {
        host.feed_output(data).rewritten.unwrap_or_else(|| data.to_vec())
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// 用一个不压缩的 deflate 块包成的 zlib 数据。
    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let len = u16::try_from(data.len()).unwrap();
        let mut out = vec![0x78, 0x01, 0x01];
        out.extend(len.to_le_bytes());
        out.extend((!len).to_le_bytes());
        out.extend(data);
        let (a, b) = data.iter().fold((1u32, 0u32), |(a, b), &byte| {
            let a = (a + u32::from(byte)) % 65521;
            (a, (b + a) % 65521)
        });
        out.extend((b << 16 | a).to_be_bytes());
        out
    }
}
