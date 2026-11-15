//! 装上 PNG 解码器以后程序传来的 PNG 图片解成 RGBA 进帧。解码器整个进程共用、装的时候不能有别的
//! 线程在用 libghostty，所以单独一个测试进程、只有这一个测试。

mod common;

use common::{base64, file_command, idle_host, idle_session};

/// 1×1 的不透明红色 PNG。
const RED_PNG: &[u8] = b"\x89PNG\x0d\x0a\x1a\x0a\x00\x00\x00\x0dIHDR\x00\x00\x00\x01\x00\x00\x00\x01\x08\x06\x00\x00\x00\x1f\x15\xc4\x89\x00\x00\x00\x0dIDATx\x9cc\xf8\xcf\xc0\xf0\x1f\x00\x05\x00\x01\xff\x89\x99=\x1d\x00\x00\x00\x00IEND\xaeB`\x82";

#[test]
fn png_images_are_decoded_once_the_decoder_is_installed() {
    // SAFETY: 这个进程里只有这一个测试，还没建过 VT。
    unsafe { runode_terminal::install_png_decoder() };
    let mut session = idle_session();
    session.feed(format!("\x1b_Ga=T,f=100,i=1,q=2;{}\x1b\\", base64(RED_PNG)).as_bytes());
    let [image] = session.frame().images.clone().try_into().unwrap();
    assert_eq!(session.image_rgba(image.id, image.generation), Some((1, 1, vec![255, 0, 0, 255])));

    // 按文件传的 PNG 由宿主读出来改成直接传，界面那份照样解得开。
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        let path = std::env::temp_dir().join(format!("runode-kitty-{}.png", std::process::id()));
        std::fs::write(&path, RED_PNG).unwrap();
        let mut host = idle_host();
        let command = file_command("a=T,t=f,f=100,i=1,q=2", &path);
        let forwarded = host.feed_output(&command).rewritten.unwrap();
        std::fs::remove_file(&path).unwrap();
        let mut session = idle_session();
        session.feed(&forwarded);
        let [image] = session.frame().images.clone().try_into().unwrap();
        assert_eq!(session.image_rgba(image.id, image.generation), Some((1, 1, vec![255, 0, 0, 255])));
    }
}
