//! 装上 PNG 解码器以后程序传来的 PNG 图片解成 RGBA 进帧。解码器整个进程共用、装的时候不能有别的
//! 线程在用 libghostty，所以单独一个测试进程、只有这一个测试。

mod common;

use common::idle_session;

/// 1×1 的不透明红色 PNG。
const RED_PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

#[test]
fn png_images_are_decoded_once_the_decoder_is_installed() {
    // SAFETY: 这个进程里只有这一个测试，还没建过 VT。
    unsafe { runode_terminal::install_png_decoder() };
    let mut session = idle_session();
    session.feed(format!("\x1b_Ga=T,f=100,i=1,q=2;{RED_PNG}\x1b\\").as_bytes());
    let [image] = session.frame().images.clone().try_into().unwrap();
    assert_eq!(session.image_rgba(image.id, image.generation), Some((1, 1, vec![255, 0, 0, 255])));
}
