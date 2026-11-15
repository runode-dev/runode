//! 会话里的程序按文件传 Kitty 图片：宿主读出文件改写成直接传，转给前端的输出里是像素，不是路径。

#![cfg(any(target_os = "macos", target_os = "linux"))]

mod common;

use common::{Peer, contains, host, script, temp_dir};
use runode_host::ClientMsg;
use runode_protocol::AttachMode;

/// 2×2 像素的 RGBA：红、绿、蓝、白。
const PIXELS: &[u8] = &[255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255];

#[test]
fn images_read_from_files_reach_the_frontend_inline() {
    let host = host();
    let mut desktop = Peer::pair(&host);
    let dir = temp_dir("kitty-file");
    let path = dir.join("pixels.rgba");
    std::fs::write(&path, PIXELS).unwrap();
    let encoded_path = base64(path.to_str().unwrap().as_bytes());
    // 先等一行输入再传图，连上以后才有输出。
    let body =
        format!(r"read go; printf '\033_Ga=T,t=f,f=32,i=1,s=2,v=2,q=2;{encoded_path}\033\\'; echo kitty-done; cat");
    let id = desktop.spawn(&script(&dir, "kitty", &body));
    let (channel, _) = desktop.attach(id, AttachMode::VtReplay);
    desktop.input(channel, b"go\r");
    let output = desktop.wait_for_output(channel, b"kitty-done");
    assert!(contains(&output, base64(PIXELS).as_bytes()), "{}", String::from_utf8_lossy(&output));
    assert!(!contains(&output, encoded_path.as_bytes()));
    desktop.send(&ClientMsg::Kill { id });
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
