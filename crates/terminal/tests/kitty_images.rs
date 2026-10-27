//! 程序用 Kitty 图形协议放到屏幕上的图片：界面这份 VT 交进帧里，宿主那份应答查询、带进快照。

mod common;

use std::time::Duration;

use common::{idle_host, idle_session};
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

/// 从文件读的图片两份 VT 各读一遍会对不上，所以不收；程序问时宿主照实回答，会改用直接传。
#[test]
fn images_read_from_files_are_refused() {
    let path = std::env::temp_dir().join(format!("runode-kitty-{}.rgba", std::process::id()));
    std::fs::write(&path, PIXELS).unwrap();
    let encoded = base64(path.to_str().unwrap().as_bytes());
    let mut session = idle_session();
    session.feed(format!("\x1b_Ga=T,t=f,f=32,i=1,s=2,v=2,q=2;{encoded}\x1b\\").as_bytes());
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

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { char::from(TABLE[(n >> (18 - 6 * i) & 63) as usize]) } else { '=' });
        }
    }
    out
}
