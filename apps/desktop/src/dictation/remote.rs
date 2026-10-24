//! 手机交给电脑识别的录音（`runode_protocol::speech`）：远程访问门禁过了的连接接到宿主所在的进程里，
//! 录音收齐后交给子进程 `runode --speech`（`run_worker`）照电脑的语音输入配置识别。识别引擎崩了只断
//! 这一段录音，宿主里的终端会话不受影响，模型也只占子进程的内存。子进程识别完不退出，下一段接着用
//! 加载好的模型，闲了卸掉模型；宿主退出、管道断了它跟着退出。

use std::{
    io::{self, Write as _},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex, PoisonError, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Context as _, bail};
use runode_config::Config;
use runode_protocol::{
    Frame, FrameKind, read_frame_limited,
    speech::{AUDIO_SAMPLE_RATE, MAX_AUDIO_SECS, SPEECH_VERSION, SpeechClientMsg, SpeechHostMsg},
    write_frame,
};

use crate::host_client::UnixStream;

const _: () = assert!(AUDIO_SAMPLE_RATE == runode_speech::SAMPLE_RATE);

/// 一段录音最多这么多字节：16 位的样本两个字节一个。
const MAX_AUDIO_BYTES: usize = (AUDIO_SAMPLE_RATE * MAX_AUDIO_SECS * 2) as usize;
/// 子进程回的一帧最多这么大：只有识别出的文字或出错的原因。
const REPLY_LIMIT: u32 = 1 << 20;
/// 子进程识别一段最多这么久，过了就当它卡死了、结束它：本机识别、交给云端和润色都算在里面。
const WORKER_TIMEOUT: Duration = Duration::from_secs(170);

/// 在跑的识别子进程，同一时刻只识别一段。
static WORKER: Mutex<Option<Worker>> = Mutex::new(None);

struct Worker {
    /// 看门狗也要拿着它，超时时结束子进程。
    child: Arc<Mutex<Child>>,
    stdin: ChildStdin,
    stdout: ChildStdout,
}

/// 给远程访问的一条新连接，另一头由后台线程按 `runode_protocol::speech` 应答。
pub fn connect() -> io::Result<UnixStream> {
    let (ours, theirs) = UnixStream::pair()?;
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
    let reply = recognize_in_worker(&pcm)
        .unwrap_or_else(|err| SpeechHostMsg::Failed { reason: format!("the speech process failed: {err:#}") });
    send(stream, &reply)
}

/// 把录音交给识别子进程，没有在跑的就拉起一个；它出了错（多半是崩了）就收掉，下一段重新拉起。
fn recognize_in_worker(pcm: &[u8]) -> anyhow::Result<SpeechHostMsg> {
    let mut slot = WORKER.lock().unwrap_or_else(PoisonError::into_inner);
    if slot.as_ref().is_some_and(|worker| !matches!(lock(&worker.child).try_wait(), Ok(None))) {
        *slot = None;
    }
    if slot.is_none() {
        *slot = Some(spawn_worker()?);
    }
    let worker = slot.as_mut().expect("spawned above");
    // 看门狗：超时还没回话就结束子进程，下面的读随即读到头、按出错收拾。
    let (answered, waiting) = mpsc::channel::<()>();
    let child = worker.child.clone();
    thread::Builder::new().name("speech-watchdog".into()).spawn(move || {
        if waiting.recv_timeout(WORKER_TIMEOUT) == Err(mpsc::RecvTimeoutError::Timeout) {
            let _ = lock(&child).kill();
        }
    })?;
    let reply = (|| {
        write_frame(&mut worker.stdin, FrameKind::Audio, 0, pcm)?;
        worker.stdin.flush()?;
        let frame = read_frame_limited(&mut worker.stdout, REPLY_LIMIT)?.context("the speech process exited")?;
        anyhow::Ok(frame.message::<SpeechHostMsg>()?)
    })();
    drop(answered);
    if reply.is_err()
        && let Some(worker) = slot.take()
    {
        let mut child = lock(&worker.child);
        let _ = child.kill();
        let _ = child.wait();
    }
    reply
}

fn lock(child: &Mutex<Child>) -> std::sync::MutexGuard<'_, Child> {
    child.lock().unwrap_or_else(PoisonError::into_inner)
}

fn spawn_worker() -> anyhow::Result<Worker> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("--speech").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    crate::subprocess::hide_window(&mut command);
    let mut child = command.spawn().context("cannot start the speech process")?;
    let stdin = child.stdin.take().context("no stdin")?;
    let stdout = child.stdout.take().context("no stdout")?;
    Ok(Worker { child: Arc::new(Mutex::new(child)), stdin, stdout })
}

/// `runode --speech`：从标准输入一段段读录音（`FrameKind::Audio`），识别后在标准输出回
/// `SpeechHostMsg::Text` 或 `Failed`；标准输入关了就退出。
pub fn run_worker() -> i32 {
    let (mut input, mut output) = (io::stdin().lock(), io::stdout().lock());
    while let Ok(Some(frame)) = read_frame_limited(&mut input, MAX_AUDIO_BYTES as u32) {
        let samples: Vec<f32> =
            frame.payload.as_chunks::<2>().0.iter().map(|b| f32::from(i16::from_le_bytes(*b)) / 32768.).collect();
        let settings = super::Settings::new(&Config::load(true));
        let reply = match super::recognize(&settings, &samples) {
            Ok(text) => SpeechHostMsg::Text { text },
            Err(err) => SpeechHostMsg::Failed { reason: format!("{err:#}") },
        };
        let Ok(frame) = Frame::control(&reply) else {
            return 1;
        };
        if write_frame(&mut output, frame.kind, frame.channel, &frame.payload).is_err() || output.flush().is_err() {
            return 1;
        }
    }
    0
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
