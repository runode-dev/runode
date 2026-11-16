//! 手机交给电脑的图片、视频（`runode_protocol::media`）：远程访问门禁过了的连接接到宿主所在的进程里，
//! 每个文件边收边写进放上传的目录里新建的子目录（`UploadOp`），回它的绝对路径，手机再让宿主把路径粘贴进终端。

use std::{
    io,
    path::{Path, PathBuf},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, bail};
use runode_protocol::{
    Frame, FrameKind,
    media::{MAX_MEDIA_BYTES, MAX_MEDIA_FRAME, MEDIA_VERSION, MediaClientMsg, MediaHostMsg},
    read_frame_limited, write_control,
};

use crate::{
    host_client::UnixStream,
    project_host::upload::{UploadOp, child, text, uploads_dir},
};

/// 认格式要看的文件头字节数；文件比它短时看整个文件。
const HEAD_LEN: usize = 12;

/// 给远程访问的一条新连接，另一头由后台线程按 `runode_protocol::media` 应答。
pub fn connect() -> io::Result<UnixStream> {
    let (ours, theirs) = runode_host::socket_pair()?;
    thread::Builder::new().name("remote-media".into()).spawn(move || {
        let root = uploads_dir(&runode_paths::Dirs::from_env());
        if let Err(err) = serve_in(&ours, root.as_deref()) {
            tracing::debug!("remote media ended: {err:#}");
        }
    })?;
    Ok(theirs)
}

/// 在 `stream` 上收文件，存进 `root`（放上传的目录，没有 home 目录时为空）。对面断开时正常返回；对面发的
/// 不合规矩（帧不对、内容比说好的长）时返回错误，连接随之断开。
fn serve_in(stream: &UnixStream, root: Option<&Path>) -> anyhow::Result<()> {
    let hello = read(stream)?.context("the phone hung up before its hello")?;
    match control(&hello)? {
        MediaClientMsg::MediaHello { version } if version == MEDIA_VERSION => {}
        MediaClientMsg::MediaHello { .. } => {
            return send(stream, &MediaHostMsg::Incompatible { version: MEDIA_VERSION });
        }
        other => bail!("expected a media hello, got {other:?}"),
    }
    send(stream, &MediaHostMsg::Ready)?;
    while let Some(frame) = read(stream)? {
        let len = match control(&frame)? {
            MediaClientMsg::Media { len } => len,
            other => bail!("expected a media file, got {other:?}"),
        };
        if len == 0 {
            return send(stream, &MediaHostMsg::Failed { reason: "the file is empty".into() });
        }
        if len > MAX_MEDIA_BYTES {
            let reason = format!("the file is larger than {} MiB", MAX_MEDIA_BYTES >> 20);
            return send(stream, &MediaHostMsg::Failed { reason });
        }
        let reply = receive(stream, root, len as usize)?;
        send(stream, &reply)?;
    }
    Ok(())
}

/// 读随 `Media { len }` 而来的 `FrameKind::Media` 帧，边收边写，回给手机的结果。存不了时照样读完这 `len`
/// 字节，连接还能接着用；对面不合规矩时返回错误，写了一半的文件删掉。
fn receive(stream: &UnixStream, root: Option<&Path>, len: usize) -> anyhow::Result<MediaHostMsg> {
    let mut sink = Sink::Head(Vec::with_capacity(HEAD_LEN.min(len)));
    let mut received = 0;
    while received < len {
        let frame = read(stream)?.context("the phone hung up in the middle of a file")?;
        if frame.kind != FrameKind::Media {
            bail!("unexpected {:?} frame in the middle of a file", frame.kind);
        }
        if received + frame.payload.len() > len {
            bail!("the file is longer than the {len} bytes announced");
        }
        received += frame.payload.len();
        sink = sink.take(root, &frame.payload);
    }
    Ok(sink.finish(root))
}

/// 收到的字节去哪。
enum Sink<'a> {
    /// 还没攒够 `HEAD_LEN` 字节的文件头；整个文件比它短时收完了仍在这里。
    Head(Vec<u8>),
    File(Upload<'a>),
    /// 存不了，余下的字节读了扔掉。
    Failed(String),
}

impl<'a> Sink<'a> {
    /// 接着收 `bytes`；攒够 `HEAD_LEN` 字节的文件头时认格式、建文件。
    fn take(self, root: Option<&'a Path>, bytes: &[u8]) -> Self {
        match self {
            Self::Head(mut head) => {
                head.extend_from_slice(bytes);
                if head.len() < HEAD_LEN {
                    return Self::Head(head);
                }
                Self::start(root, &head)
            }
            Self::File(mut upload) => match upload.append(bytes) {
                Ok(()) => Self::File(upload),
                Err(err) => Self::Failed(format!("cannot save the file: {err}")),
            },
            failed @ Self::Failed(_) => failed,
        }
    }

    /// 收完了，回给手机的结果。
    fn finish(self, root: Option<&'a Path>) -> MediaHostMsg {
        match self {
            Self::Head(head) => Self::start(root, &head).finish(root),
            Self::File(upload) => MediaHostMsg::Saved { path: upload.keep() },
            Self::Failed(reason) => MediaHostMsg::Failed { reason },
        }
    }

    /// 按文件头 `head` 建文件，不会是 `Head`。
    fn start(root: Option<&'a Path>, head: &[u8]) -> Self {
        match Upload::begin(root, head) {
            Ok(upload) => Self::File(upload),
            Err(reason) => Self::Failed(reason),
        }
    }
}

/// 一个正在写的文件。没 `keep` 就丢掉时删掉这次的子目录，不留半截的文件。
struct Upload<'a> {
    root: Option<&'a Path>,
    dir: PathBuf,
    path: PathBuf,
    offset: u64,
    kept: bool,
}

impl<'a> Upload<'a> {
    /// 按文件头 `head` 认出格式，在 `root` 下新建子目录和文件，写进 `head`；认不出、存不了时回给手机看的原因。
    fn begin(root: Option<&'a Path>, head: &[u8]) -> Result<Self, String> {
        let (kind, extension) = format(head).ok_or("the file is not a PNG, JPEG, GIF, WebP, MP4 or MOV file")?;
        let failed = |err: io::Error| format!("cannot save the file: {err}");
        let dir = UploadOp::Begin.run(root, &[]).map_err(failed)?.ok_or("cannot save the file: no upload directory")?;
        let millis = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis());
        let path = child(&dir, &format!("{kind}-{millis}.{extension}"));
        let mut upload = Self { root, dir, path, offset: 0, kept: false };
        upload.append(head).map_err(failed)?;
        Ok(upload)
    }

    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        UploadOp::Chunk { path: self.path.clone(), offset: self.offset }.run(self.root, bytes)?;
        self.offset += bytes.len() as u64;
        Ok(())
    }

    /// 留下写好的文件，回它的绝对路径。
    fn keep(mut self) -> String {
        self.kept = true;
        text(&self.path)
    }
}

impl Drop for Upload<'_> {
    fn drop(&mut self) {
        if !self.kept {
            let _ = UploadOp::Discard { dir: self.dir.clone() }.run(self.root, &[]);
        }
    }
}

/// 按文件头认出的种类（存成的文件名的前缀）和扩展名。
fn format(head: &[u8]) -> Option<(&'static str, &'static str)> {
    let image = if head.starts_with(b"\x89PNG") {
        "png"
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else if head.starts_with(b"GIF8") {
        "gif"
    } else if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        "webp"
    } else if head.len() >= 12 && &head[4..8] == b"ftyp" {
        // HEIF 系的图片（HEIC、AVIF）和佳能 CR3 原始图也是 ISO 媒体文件，按主品牌认出来挡掉。
        return match &head[8..12] {
            b"qt  " => Some(("video", "mov")),
            b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"hevm" | b"hevs" | b"mif1" | b"msf1"
            | b"avif" | b"avis" | b"avci" | b"avcs" | b"crx " => None,
            _ => Some(("video", "mp4")),
        };
    } else {
        return None;
    };
    Some(("image", image))
}

/// 读一帧；对面断开时为 `None`。
fn read(stream: &UnixStream) -> anyhow::Result<Option<Frame>> {
    Ok(read_frame_limited(&mut &*stream, MAX_MEDIA_FRAME)?)
}

fn control(frame: &Frame) -> anyhow::Result<MediaClientMsg> {
    if frame.kind != FrameKind::Control {
        bail!("expected a control frame, got {:?}", frame.kind);
    }
    Ok(frame.message()?)
}

fn send(stream: &UnixStream, message: &MediaHostMsg) -> anyhow::Result<()> {
    Ok(write_control(&mut &*stream, message)?)
}

#[cfg(test)]
mod tests {
    use std::{fs, thread::JoinHandle};

    use runode_protocol::write_frame;

    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nnot really a picture";
    const MP4: &[u8] = b"\0\0\0\x18ftypisom\0\0\x02\0isomiso2 and then some frames";
    const MOV: &[u8] = b"\0\0\0\x14ftypqt  \0\0\x02\0qt   and then some frames";

    /// 这个测试自己的空目录、在它上面跑着的服务的另一头，和跑服务的线程。
    fn start(name: &str) -> (PathBuf, UnixStream, JoinHandle<anyhow::Result<()>>) {
        let root = std::env::temp_dir().join(format!("runode-media-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let (ours, theirs) = runode_host::socket_pair().unwrap();
        let uploads = root.join("uploads");
        let server = thread::spawn(move || serve_in(&ours, Some(&uploads)));
        (root, theirs, server)
    }

    fn reply(stream: &UnixStream) -> MediaHostMsg {
        read(stream).unwrap().unwrap().message().unwrap()
    }

    fn hello(stream: &UnixStream, version: u32) {
        write_control(&mut &*stream, &MediaClientMsg::MediaHello { version }).unwrap();
    }

    fn ready(name: &str) -> (PathBuf, UnixStream, JoinHandle<anyhow::Result<()>>) {
        let (root, phone, server) = start(name);
        hello(&phone, MEDIA_VERSION);
        assert_eq!(reply(&phone), MediaHostMsg::Ready);
        (root, phone, server)
    }

    /// 发一个文件，分成两帧。
    fn send_file(stream: &UnixStream, bytes: &[u8]) {
        write_control(&mut &*stream, &MediaClientMsg::Media { len: bytes.len() as u32 }).unwrap();
        let (head, tail) = bytes.split_at(bytes.len() / 2);
        write_frame(&mut &*stream, FrameKind::Media, 0, head).unwrap();
        write_frame(&mut &*stream, FrameKind::Media, 0, tail).unwrap();
    }

    fn saved(stream: &UnixStream) -> PathBuf {
        match reply(stream) {
            MediaHostMsg::Saved { path } => PathBuf::from(path),
            other => panic!("expected the file to be saved: {other:?}"),
        }
    }

    #[test]
    fn another_version_is_told_and_dropped() {
        let (root, phone, _) = start("version");
        hello(&phone, MEDIA_VERSION + 1);
        assert_eq!(reply(&phone), MediaHostMsg::Incompatible { version: MEDIA_VERSION });
        assert!(read(&phone).unwrap().is_none());
        let _ = fs::remove_dir_all(&root);
    }

    /// 文件的大小有上限，手机不能让电脑一直写下去。
    #[test]
    fn a_file_over_the_limit_fails() {
        let (root, phone, _) = ready("limit");
        write_control(&mut &phone, &MediaClientMsg::Media { len: MAX_MEDIA_BYTES + 1 }).unwrap();
        let reason = format!("the file is larger than {} MiB", MAX_MEDIA_BYTES >> 20);
        assert_eq!(reply(&phone), MediaHostMsg::Failed { reason });
        assert!(read(&phone).unwrap().is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_empty_file_fails() {
        let (root, phone, _) = ready("empty");
        write_control(&mut &phone, &MediaClientMsg::Media { len: 0 }).unwrap();
        assert!(matches!(reply(&phone), MediaHostMsg::Failed { .. }));
        assert!(read(&phone).unwrap().is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_that_is_not_an_image_or_video_fails() {
        let (root, phone, _) = ready("unknown");
        send_file(&phone, b"just some text");
        assert!(matches!(reply(&phone), MediaHostMsg::Failed { .. }));
        // 连接还能接着用。
        send_file(&phone, PNG);
        saved(&phone);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_image_is_saved_privately_under_the_uploads() {
        let (root, phone, _) = ready("png");
        send_file(&phone, PNG);
        let path = saved(&phone);
        assert!(path.is_absolute() && path.starts_with(root.join("uploads")), "{}", path.display());
        assert_eq!(path.extension().unwrap(), "png");
        assert!(path.file_name().unwrap().to_string_lossy().starts_with("image-"));
        assert_eq!(fs::read(&path).unwrap(), PNG);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn videos_are_saved_as_mp4_or_mov_by_their_brand() {
        let (root, phone, _) = ready("video");
        send_file(&phone, MP4);
        let mp4 = saved(&phone);
        send_file(&phone, MOV);
        let mov = saved(&phone);
        assert_eq!((mp4.extension().unwrap(), mov.extension().unwrap()), ("mp4".as_ref(), "mov".as_ref()));
        assert!(mp4.file_name().unwrap().to_string_lossy().starts_with("video-"));
        assert_eq!((fs::read(&mp4).unwrap(), fs::read(&mov).unwrap()), (MP4.to_vec(), MOV.to_vec()));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn heif_images_are_not_taken_for_videos() {
        let (root, phone, _) = ready("heif");
        send_file(&phone, b"\0\0\0\x18ftypheic\0\0\0\0mif1heic");
        assert!(matches!(reply(&phone), MediaHostMsg::Failed { .. }));
        send_file(&phone, b"\0\0\0\x1cftypavif\0\0\0\0avifmif1miaf");
        assert!(matches!(reply(&phone), MediaHostMsg::Failed { .. }));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_shorter_than_the_head_is_still_recognised() {
        let (root, phone, _) = ready("short");
        send_file(&phone, b"GIF89a");
        assert_eq!(fs::read(saved(&phone)).unwrap(), b"GIF89a");
        send_file(&phone, b"short");
        assert!(matches!(reply(&phone), MediaHostMsg::Failed { .. }));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn one_connection_sends_several_files() {
        let (root, phone, _) = ready("several");
        let jpeg = b"\xFF\xD8\xFF\xE0 a jpeg";
        let webp = b"RIFF\x10\0\0\0WEBPVP8 ";
        send_file(&phone, jpeg);
        let first = saved(&phone);
        send_file(&phone, webp);
        let second = saved(&phone);
        assert_ne!(first.parent(), second.parent());
        assert_eq!((fs::read(&first).unwrap(), fs::read(&second).unwrap()), (jpeg.to_vec(), webp.to_vec()));
        assert_eq!(first.extension().unwrap(), "jpg");
        assert_eq!(second.extension().unwrap(), "webp");
        let _ = fs::remove_dir_all(&root);
    }

    /// 文件头被拆在几帧里、文件比一帧长时，按帧的顺序接起来。
    #[test]
    fn frames_are_joined_in_order() {
        let (root, phone, _) = ready("frames");
        let mut bytes = MP4[..HEAD_LEN].to_vec();
        bytes.extend((0..(MAX_MEDIA_FRAME as usize * 2 + 1000)).map(|i| (i % 251) as u8));
        write_control(&mut &phone, &MediaClientMsg::Media { len: bytes.len() as u32 }).unwrap();
        // 电脑存完才回话，一路写下去不会两头互等。
        write_frame(&mut &phone, FrameKind::Media, 0, &bytes[..3]).unwrap();
        write_frame(&mut &phone, FrameKind::Media, 0, &bytes[3..8]).unwrap();
        for chunk in bytes[8..].chunks(MAX_MEDIA_FRAME as usize) {
            write_frame(&mut &phone, FrameKind::Media, 0, chunk).unwrap();
        }
        let path = saved(&phone);
        assert_eq!(path.extension().unwrap(), "mp4");
        assert!(fs::read(&path).unwrap() == bytes);
        let _ = fs::remove_dir_all(&root);
    }

    /// 发到一半断开时，没收完的文件连同这次的子目录删掉。
    #[test]
    fn a_hang_up_in_the_middle_discards_the_file() {
        let (root, phone, server) = ready("hangup");
        write_control(&mut &phone, &MediaClientMsg::Media { len: 1000 }).unwrap();
        write_frame(&mut &phone, FrameKind::Media, 0, PNG).unwrap();
        drop(phone);
        assert!(server.join().unwrap().is_err());
        // 放上传的目录是收到文件头后建子目录时建的，里面这时空了。
        let uploads = root.join("uploads");
        assert_eq!(fs::read_dir(&uploads).unwrap().count(), 0, "{}", uploads.display());
        let _ = fs::remove_dir_all(&root);
    }

    /// 内容比说好的长时数据已经对不齐，断开。
    #[test]
    fn more_bytes_than_announced_hang_up() {
        let (root, phone, _) = ready("longer");
        write_control(&mut &phone, &MediaClientMsg::Media { len: 4 }).unwrap();
        write_frame(&mut &phone, FrameKind::Media, 0, PNG).unwrap();
        assert!(read(&phone).unwrap().is_none());
        let _ = fs::remove_dir_all(&root);
    }

    /// 一帧比 `MAX_MEDIA_FRAME` 长时断开：帧头报的长度就超了，不等载荷。
    #[test]
    fn a_frame_over_the_limit_hangs_up() {
        let (root, phone, server) = ready("frame");
        write_control(&mut &phone, &MediaClientMsg::Media { len: MAX_MEDIA_FRAME * 2 }).unwrap();
        let mut header = (MAX_MEDIA_FRAME + 1).to_le_bytes().to_vec();
        header.push(FrameKind::Media as u8);
        header.extend(0u32.to_le_bytes());
        io::Write::write_all(&mut &phone, &header).unwrap();
        assert!(server.join().unwrap().is_err());
        assert!(read(&phone).unwrap().is_none());
        let _ = fs::remove_dir_all(&root);
    }
}
