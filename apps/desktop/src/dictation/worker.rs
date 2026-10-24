//! 识别子进程 `runode --speech`（`run`）：本机的语音输入和手机交来的录音都交给它照语音输入配置识别。
//! 识别引擎崩了只断这一段录音，窗口和宿主里的终端会话不受影响。子进程识别完不退出，下一段接着用加载好的
//! 模型，闲了 `IDLE_END` 就结束它，模型占的内存随进程一起还给系统；拉起它的进程退出、管道断了它跟着退出。

use std::{
    io::{self, BufRead, BufReader, Read as _, Write as _},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{Arc, Mutex, PoisonError, TryLockError, mpsc},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context as _, bail};
use runode_config::Config;
use runode_protocol::{
    FrameKind, read_frame_limited,
    speech::{AUDIO_SAMPLE_RATE, MAX_AUDIO_SECS, SpeechHostMsg},
    write_frame,
};

const _: () = assert!(AUDIO_SAMPLE_RATE == runode_speech::SAMPLE_RATE);

/// 一段录音最多这么多字节：16 位的样本两个字节一个。
pub const MAX_AUDIO_BYTES: usize = (AUDIO_SAMPLE_RATE * MAX_AUDIO_SECS * 2) as usize;
/// 子进程回的一行最多这么长：只有识别出的文字或出错的原因。
const REPLY_LIMIT: u64 = 1 << 20;
/// 子进程回话的行以它开头：sherpa-onnx 和 onnxruntime 也往标准输出写字，不能按帧读。
const REPLY_MARK: &[u8] = b"runode-speech-reply ";
/// 子进程这么久没用就结束：FireRedASR2 加载后占一个多 GB 内存，用一次就一直占着不划算。
const IDLE_END: Duration = Duration::from_secs(180);

/// 在跑的识别子进程，同一时刻只识别一段。
static WORKER: Mutex<Option<Worker>> = Mutex::new(None);

struct Worker {
    /// 看门狗也要拿着它，超时时结束子进程。
    child: Arc<Mutex<Child>>,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// 上次交给它录音的时刻。
    used: Instant,
}

/// 把录音（`AUDIO_SAMPLE_RATE` 的 16 位小端单声道）交给识别子进程，没有在跑的就拉起一个；它出了错
/// （多半是崩了）就收掉，下一段重新拉起。`timeout` 内没有结果就当子进程卡死了、结束它，排队等前一段、
/// 识别、交给云端和润色都算在里面。
pub fn recognize(pcm: &[u8], timeout: Duration) -> anyhow::Result<SpeechHostMsg> {
    let deadline = Instant::now() + timeout;
    let mut slot = WORKER.lock().unwrap_or_else(PoisonError::into_inner);
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        bail!("timed out waiting for another recording to be recognized");
    }
    if let Some(worker) = slot.take_if(|worker| !matches!(lock(&worker.child).try_wait(), Ok(None))) {
        end_worker(worker);
    }
    if slot.is_none() {
        *slot = Some(spawn()?);
    }
    let worker = slot.as_mut().expect("spawned above");
    // 看门狗：超时还没回话就结束子进程，下面的读随即读到头、按出错收拾。
    let (answered, waiting) = mpsc::channel::<()>();
    let child = worker.child.clone();
    thread::Builder::new().name("speech-watchdog".into()).spawn(move || {
        if waiting.recv_timeout(left) == Err(mpsc::RecvTimeoutError::Timeout) {
            let _ = lock(&child).kill();
        }
    })?;
    let reply = (|| {
        write_frame(&mut worker.stdin, FrameKind::Audio, 0, pcm)?;
        worker.stdin.flush()?;
        read_reply(&mut worker.stdout)
    })();
    worker.used = Instant::now();
    drop(answered);
    if reply.is_err()
        && let Some(worker) = slot.take()
    {
        end_worker(worker);
    }
    reply
}

/// 结束在跑的子进程；正在识别的话不等，留给 `end_when_idle` 结束。
pub fn end() {
    let mut slot = match WORKER.try_lock() {
        Ok(slot) => slot,
        Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(TryLockError::WouldBlock) => return,
    };
    if let Some(worker) = slot.take() {
        end_worker(worker);
    }
}

fn end_worker(worker: Worker) {
    let mut child = lock(&worker.child);
    let _ = child.kill();
    let _ = child.wait();
}

/// `child` 闲了 `IDLE_END` 就结束它；它已经不是在跑的那个子进程了就返回。
fn end_when_idle(child: Arc<Mutex<Child>>) {
    let mut wait = IDLE_END;
    loop {
        thread::sleep(wait);
        let mut slot = WORKER.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(worker) = slot.as_ref().filter(|worker| Arc::ptr_eq(&worker.child, &child)) else {
            return;
        };
        if let Some(left) = IDLE_END.checked_sub(worker.used.elapsed()).filter(|left| !left.is_zero()) {
            wait = left;
            continue;
        }
        if let Some(worker) = slot.take() {
            end_worker(worker);
        }
        return;
    }
}

/// 读到 `REPLY_MARK` 开头的那行为止，原生库写的别的字跳过。
fn read_reply(stdout: &mut impl BufRead) -> anyhow::Result<SpeechHostMsg> {
    let mut line = Vec::new();
    loop {
        line.clear();
        if stdout.take(REPLY_LIMIT).read_until(b'\n', &mut line)? == 0 {
            bail!("the speech process exited");
        }
        if let Some(at) = line.windows(REPLY_MARK.len()).position(|w| w == REPLY_MARK) {
            return Ok(serde_json::from_slice(&line[at + REPLY_MARK.len()..])?);
        }
    }
}

fn lock(child: &Mutex<Child>) -> std::sync::MutexGuard<'_, Child> {
    child.lock().unwrap_or_else(PoisonError::into_inner)
}

fn spawn() -> anyhow::Result<Worker> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("--speech").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    crate::subprocess::hide_window(&mut command);
    let mut child = command.spawn().context("cannot start the speech process")?;
    let stdin = child.stdin.take().context("no stdin")?;
    let stdout = BufReader::new(child.stdout.take().context("no stdout")?);
    let worker = Worker { child: Arc::new(Mutex::new(child)), stdin, stdout, used: Instant::now() };
    let child = worker.child.clone();
    if let Err(err) = thread::Builder::new().name("speech-reaper".into()).spawn(move || end_when_idle(child)) {
        end_worker(worker);
        return Err(err.into());
    }
    Ok(worker)
}

/// `runode --speech`：从标准输入一段段读录音（`FrameKind::Audio`），识别后在标准输出回一行
/// `REPLY_MARK` 加 JSON 的 `SpeechHostMsg::Text` 或 `Failed`；标准输入关了就退出。
pub fn run() -> i32 {
    let (mut input, mut output) = (io::stdin().lock(), io::stdout().lock());
    while let Ok(Some(frame)) = read_frame_limited(&mut input, MAX_AUDIO_BYTES as u32) {
        let samples: Vec<f32> =
            frame.payload.as_chunks::<2>().0.iter().map(|b| f32::from(i16::from_le_bytes(*b)) / 32768.).collect();
        let settings = super::Settings::new(&Config::load(true));
        let reply = match super::recognize(&settings, &samples) {
            Ok(text) => SpeechHostMsg::Text { text },
            Err(err) => SpeechHostMsg::Failed { reason: format!("{err:#}") },
        };
        let Ok(json) = serde_json::to_vec(&reply) else {
            return 1;
        };
        // 一次写完整行，免得夹进原生库写的字；JSON 里没有换行。
        let line = [REPLY_MARK, &json, b"\n"].concat();
        if output.write_all(&line).is_err() || output.flush().is_err() {
            return 1;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 原生库往子进程的标准输出写的字（没换行也一样）不能弄坏回话。
    #[test]
    fn a_reply_is_found_among_native_output() {
        let text = SpeechHostMsg::Text { text: "你好".into() };
        let line = [b"onnxruntime says hi\npartial".as_slice(), REPLY_MARK, &serde_json::to_vec(&text).unwrap(), b"\n"];
        assert_eq!(read_reply(&mut line.concat().as_slice()).unwrap(), text);
        assert!(read_reply(&mut b"no reply\n".as_slice()).is_err());
    }
}
