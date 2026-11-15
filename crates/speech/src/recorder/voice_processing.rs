//! macOS 上用系统的语音处理录麦克风：`AVAudioEngine` 的输入节点开着 voice processing 时，系统从
//! 麦克风里去掉此刻本机放出来的声音。开着时系统会压低别的 App 的声音，这里设成压得最少。
//!
//! `AVAudioEngine` 不能跨线程用，和 cpal 的流一样留在录音线程上，drop 时停下。

use std::{
    ptr::NonNull,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

use anyhow::Context;
use block2::RcBlock;
use objc2::{rc::Retained, runtime::Bool};
use objc2_avf_audio::{
    AVAudioCommonFormat, AVAudioEngine, AVAudioInputNode, AVAudioPCMBuffer, AVAudioTime,
    AVAudioVoiceProcessingOtherAudioDuckingConfiguration, AVAudioVoiceProcessingOtherAudioDuckingLevel,
};
use objc2_foundation::{NSError, NSOperatingSystemVersion, NSProcessInfo};

use super::{Shared, Track};

/// 每次交给回调的帧数（系统只当建议）。
const TAP_FRAMES: u32 = 4096;

/// 开着的引擎，drop 时摘掉回调、停下。
pub(super) struct Engine {
    engine: Retained<AVAudioEngine>,
    input: Retained<AVAudioInputNode>,
}

impl Drop for Engine {
    fn drop(&mut self) {
        // SAFETY: 引擎和节点都在创建它们的录音线程上用；回调装上了才有 `Engine`，停下没开起来的引擎无害。
        unsafe {
            self.input.removeTapOnBus(0);
            self.engine.stop();
        }
    }
}

/// 开着语音处理开始录默认的麦克风，交出开着的引擎和它写的音轨。没有麦克风、开不了语音处理时报错。
pub(super) fn open() -> anyhow::Result<(Engine, Shared)> {
    // SAFETY: 都是在录音线程上调 AVFAudio 的方法；会抛 Objective-C 异常的 `installTap` 前先核对过
    // 格式：交给它的就是节点自己报的格式，采样率和声道数都不为 0。
    unsafe {
        let engine = AVAudioEngine::new();
        let input = engine.inputNode();
        input.setVoiceProcessingEnabled_error(true).map_err(describe).context("cannot enable voice processing")?;
        // 压低别的声音的设置 macOS 14 才有，老系统上调它会崩。
        if NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(NSOperatingSystemVersion {
            majorVersion: 14,
            minorVersion: 0,
            patchVersion: 0,
        }) {
            input.setVoiceProcessingOtherAudioDuckingConfiguration(
                AVAudioVoiceProcessingOtherAudioDuckingConfiguration {
                    enableAdvancedDucking: Bool::NO,
                    duckingLevel: AVAudioVoiceProcessingOtherAudioDuckingLevel::Min,
                },
            );
        }
        // 开了语音处理之后格式会变，所以这时才读。
        let format = input.outputFormatForBus(0);
        let rate = format.sampleRate();
        anyhow::ensure!(rate >= 1. && format.channelCount() > 0, "no microphone");
        anyhow::ensure!(
            format.commonFormat() == AVAudioCommonFormat::PCMFormatFloat32,
            "unsupported sample format {:?}",
            format.commonFormat()
        );
        let track = Shared::new(Mutex::new(Track::new(rate.round() as u32, false)));
        let tap = tap(track.clone());
        input.installTapOnBus_bufferSize_format_block(0, TAP_FRAMES, Some(&format), RcBlock::as_ptr(&tap));
        // 装上回调之后才交给 `Engine`：它 drop 时摘回调。
        let engine = Engine { engine, input };
        engine.engine.prepare();
        engine.engine.startAndReturnError().map_err(describe).context("cannot start the audio engine")?;
        Ok((engine, track))
    }
}

type TapBlock = RcBlock<dyn Fn(NonNull<AVAudioPCMBuffer>, NonNull<AVAudioTime>)>;

/// 把交来的样本写进 `track` 的回调。开了语音处理的输入节点可能报好几个声道，只有第 0 声道是处理过
/// 的，所以只取它。
fn tap(track: Shared) -> TapBlock {
    // 第一块样本的 `Instant` 和主机时刻（秒）：之后的样本按主机时刻离它多远排在时间轴上。
    let origin = Mutex::new(None::<(Instant, f64)>);
    RcBlock::new(move |buffer: NonNull<AVAudioPCMBuffer>, when: NonNull<AVAudioTime>| {
        // SAFETY: 系统交来的缓冲和时刻在回调期间有效；格式是 float32，`floatChannelData` 指向每个声道
        // 的样本，相邻两帧隔 `stride` 个 float。
        unsafe {
            let buffer = buffer.as_ref();
            let when = when.as_ref();
            let channels = buffer.floatChannelData();
            if channels.is_null() {
                return;
            }
            let samples = (*channels).as_ptr();
            let frames = buffer.frameLength() as usize;
            let stride = buffer.stride().max(1);
            let at = if when.isHostTimeValid() {
                let secs = AVAudioTime::secondsForHostTime(when.hostTime());
                let mut origin = origin.lock().unwrap_or_else(PoisonError::into_inner);
                let (instant, base) = *origin.get_or_insert((Instant::now(), secs));
                instant + Duration::try_from_secs_f64(secs - base).unwrap_or_default()
            } else {
                Instant::now()
            };
            let mut track = track.lock().unwrap_or_else(PoisonError::into_inner);
            track.push(at, (0..frames).map(|i| *samples.add(i * stride)));
        }
    })
}

fn describe(err: Retained<NSError>) -> anyhow::Error {
    anyhow::anyhow!("{}", err.localizedDescription())
}
