//! 手机交给电脑识别的录音（`runode_protocol::speech`）：远程访问门禁过了的连接接到宿主所在的进程里，
//! 录音收齐后交给识别子进程（`super::worker`）。

use std::{io, thread, time::Duration};

use anyhow::{Context as _, bail};
use runode_config::Config;
use runode_protocol::{
    Frame, FrameKind, read_frame_limited,
    speech::{MAX_AUDIO_SECS, SPEECH_VERSION, SpeechClientMsg, SpeechHostMsg},
    write_frame,
};

use super::worker::{self, MAX_AUDIO_BYTES};
use crate::host_client::UnixStream;

/// 一段录音交过来后最多等这么久识别，要比手机等回话的时限短，手机才收得到出错的原因。
const RECOGNIZE_TIMEOUT: Duration = Duration::from_secs(170);

/// 给远程访问的一条新连接，另一头由后台线程按 `runode_protocol::speech` 应答。
pub fn connect() -> io::Result<UnixStream> {
    let (ours, theirs) = runode_host::socket_pair()?;
    thread::Builder::new().name("remote-speech".into()).spawn(move || {
        if let Err(err) = serve(&ours) {
            tracing::debug!("remote speech ended: {err:#}");
        }
    })?;
    Ok(theirs)
}

fn serve(stream: &UnixStream) -> anyhow::Result<()> {
    let hello = read(stream)?;
    match hello.message::<SpeechClientMsg>()? {
        SpeechClientMsg::SpeechHello { version } if version == SPEECH_VERSION => {}
        SpeechClientMsg::SpeechHello { .. } => {
            return send(stream, &SpeechHostMsg::Incompatible { version: SPEECH_VERSION });
        }
        other => bail!("expected a speech hello, got {other:?}"),
    }
    let settings = super::Settings::new(&Config::load(true));
    let installed = settings.available();
    send(stream, &SpeechHostMsg::Ready { installed })?;
    let mut pcm = Vec::new();
    loop {
        let frame = read(stream)?;
        match frame.kind {
            FrameKind::Audio if pcm.len() + frame.payload.len() > MAX_AUDIO_BYTES => {
                let reason = format!("the recording is longer than {MAX_AUDIO_SECS} seconds");
                return send(stream, &SpeechHostMsg::Failed { reason });
            }
            FrameKind::Audio => pcm.extend_from_slice(&frame.payload),
            FrameKind::Control if frame.message::<SpeechClientMsg>()? == SpeechClientMsg::Finish => break,
            kind => bail!("unexpected {kind:?} frame"),
        }
    }
    if !installed {
        let reason = format!("the speech model {} is not downloaded on this computer", settings.model().name());
        return send(stream, &SpeechHostMsg::Failed { reason });
    }
    let reply = worker::recognize(&pcm, RECOGNIZE_TIMEOUT)
        .unwrap_or_else(|err| SpeechHostMsg::Failed { reason: format!("the speech process failed: {err:#}") });
    send(stream, &reply)
}

fn read(stream: &UnixStream) -> anyhow::Result<Frame> {
    // 录音帧不会比一整段录音还长。
    read_frame_limited(&mut &*stream, MAX_AUDIO_BYTES as u32)?.context("the phone hung up")
}

fn send(stream: &UnixStream, message: &SpeechHostMsg) -> anyhow::Result<()> {
    let frame = Frame::control(message)?;
    write_frame(&mut &*stream, frame.kind, frame.channel, &frame.payload)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(stream: &UnixStream) -> SpeechHostMsg {
        read(stream).unwrap().message().unwrap()
    }

    fn hello(stream: &UnixStream, version: u32) {
        let frame = Frame::control(&SpeechClientMsg::SpeechHello { version }).unwrap();
        write_frame(&mut &*stream, frame.kind, frame.channel, &frame.payload).unwrap();
    }

    #[test]
    fn another_version_is_told_and_dropped() {
        let phone = connect().unwrap();
        hello(&phone, SPEECH_VERSION + 1);
        assert_eq!(reply(&phone), SpeechHostMsg::Incompatible { version: SPEECH_VERSION });
        assert!(read_frame_limited(&mut &phone, 1024).unwrap().is_none());
    }

    /// 录音的长度有上限，手机不能让电脑一直攒着。
    #[test]
    fn a_recording_over_the_limit_fails() {
        let phone = connect().unwrap();
        hello(&phone, SPEECH_VERSION);
        assert!(matches!(reply(&phone), SpeechHostMsg::Ready { .. }));
        let chunk = vec![0u8; MAX_AUDIO_BYTES / 2 + 2];
        write_frame(&mut &phone, FrameKind::Audio, 0, &chunk).unwrap();
        write_frame(&mut &phone, FrameKind::Audio, 0, &chunk).unwrap();
        assert!(matches!(reply(&phone), SpeechHostMsg::Failed { .. }));
    }
}
