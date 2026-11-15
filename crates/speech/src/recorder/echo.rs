//! 用回采到的外放声音把它的回声从麦克风录音里消掉，交给 sonora（WebRTC AEC3 的移植）。整段录完
//! 才一次处理：两条音轨先都重采样到 `SAMPLE_RATE`，按采集时刻对齐后逐帧交给它。

use sonora::{
    AudioProcessing, Config, StreamConfig,
    config::{EchoCanceller, HighPassFilter},
};

use super::Track;
use crate::SAMPLE_RATE;

/// 参考音轨的峰值低于它就当外放没有声音，录音原样交回。
const AUDIBLE: f32 = 1e-4;

/// sonora 一帧的长度是一秒的这么多分之一，采样率要能被它整除。
const FRAMES_PER_SEC: u32 = 100;

const _: () = assert!(SAMPLE_RATE.is_multiple_of(FRAMES_PER_SEC));

/// 一帧的样本数。
const FRAME_LEN: usize = (SAMPLE_RATE / FRAMES_PER_SEC) as usize;

/// 从 `mic` 里消掉 `reference` 的回声，交回 `SAMPLE_RATE` 上和 `mic` 一样长的音频。外放没有声音，
/// 或者消回声失败时，交回只重采样过的 `mic`。
pub(super) fn cancel(mic: Track, reference: Track) -> Vec<f32> {
    let mic = mic.into_resampled();
    if reference.samples.iter().all(|s| s.abs() < AUDIBLE) {
        return mic.samples;
    }
    let reference = reference.into_resampled();
    match process(&mic.samples, &align(&mic, &reference)) {
        Ok(cleaned) => cleaned,
        Err(err) => {
            tracing::warn!("消回声失败，交出原样的录音：{err:#}");
            mic.samples
        }
    }
}

/// 把 `reference` 挪到 `mic` 的时间轴上：参考晚开始的在前面补 0，早开始的去掉多出的开头。
fn align(mic: &Track, reference: &Track) -> Vec<f32> {
    let (Some(mic_start), Some(reference_start)) = (mic.start, reference.start) else {
        return reference.samples.clone();
    };
    let rate = f64::from(reference.rate);
    if reference_start >= mic_start {
        let lead = (reference_start.duration_since(mic_start).as_secs_f64() * rate).round() as usize;
        let mut aligned = vec![0.; lead];
        aligned.extend_from_slice(&reference.samples);
        aligned
    } else {
        let skip = (mic_start.duration_since(reference_start).as_secs_f64() * rate).round() as usize;
        reference.samples.get(skip..).unwrap_or_default().to_vec()
    }
}

/// 按 `FRAMES_PER_SEC` 逐帧先交参考、再交麦克风。参考已经对齐在麦克风的时间轴上，所以告诉它的延迟是 0，
/// 剩下的声学延迟由 AEC3 自己估。参考比麦克风短的部分当静音，末尾不满一帧的补 0 处理完再截回原长。
fn process(mic: &[f32], reference: &[f32]) -> anyhow::Result<Vec<f32>> {
    let config = Config {
        echo_canceller: Some(EchoCanceller::default()),
        high_pass_filter: Some(HighPassFilter::default()),
        ..Config::default()
    };
    let mut apm = AudioProcessing::builder()
        .config(config)
        .capture_config(StreamConfig::new(SAMPLE_RATE, 1))
        .render_config(StreamConfig::new(SAMPLE_RATE, 1))
        .build();
    let mut capture = [0.; FRAME_LEN];
    let mut cleaned = [0.; FRAME_LEN];
    let mut render = [0.; FRAME_LEN];
    let mut render_out = [0.; FRAME_LEN];
    let mut out = Vec::with_capacity(mic.len().next_multiple_of(FRAME_LEN));
    for offset in (0..mic.len()).step_by(FRAME_LEN) {
        fill(&mut render, reference, offset);
        apm.process_render_f32(&[&render], &mut [&mut render_out])?;
        // 只在超出它收得下的范围、被夹住时报错，0 不会。
        let _ = apm.set_stream_delay_ms(0);
        fill(&mut capture, mic, offset);
        apm.process_capture_f32(&[&capture], &mut [&mut cleaned])?;
        out.extend_from_slice(&cleaned);
    }
    out.truncate(mic.len());
    Ok(out)
}

/// 把 `from` 里从 `offset` 起的一帧抄进 `frame`，不够的补 0。
fn fill(frame: &mut [f32], from: &[f32], offset: usize) {
    let available = from.get(offset..).unwrap_or_default();
    let n = available.len().min(frame.len());
    frame[..n].copy_from_slice(&available[..n]);
    frame[n..].fill(0.);
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{super::resample, *};

    const SECS: usize = 10;
    /// 只看最后这几秒：AEC3 前几秒还在收敛。
    const TAIL_SECS: usize = 3;

    /// 可复现的白噪声，取值在 [-1, 1]。
    fn noise(len: usize, seed: u32) -> Vec<f32> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as f32 / u32::MAX as f32 * 2. - 1.
            })
            .collect()
    }

    /// 像说话的近端声音：基频在 120–220 Hz 之间慢慢变的一串谐波，按每秒三个音节的节奏起落。
    fn speech(len: usize, rate: u32) -> Vec<f32> {
        let rate = rate as f32;
        let mut phase = 0f32;
        (0..len)
            .map(|n| {
                let t = n as f32 / rate;
                let f0 = 170. + 50. * (std::f32::consts::TAU * 0.3 * t).sin();
                phase += std::f32::consts::TAU * f0 / rate;
                let voice: f32 = (1..=15).map(|k| (phase * k as f32).sin() / k as f32).sum();
                let envelope = (std::f32::consts::PI * 3. * t).sin().abs();
                0.15 * voice * envelope
            })
            .collect()
    }

    /// 外放经房间回到麦克风：晚 `delay`，低通滤过，衰减一半。
    fn echo_of(reference: &[f32], rate: u32, delay: Duration) -> Vec<f32> {
        let delay = (delay.as_secs_f64() * f64::from(rate)) as usize;
        let mut low = 0f32;
        (0..reference.len())
            .map(|n| {
                let x = if n >= delay { reference[n - delay] } else { 0. };
                low = 0.6 * low + 0.4 * x;
                0.5 * low
            })
            .collect()
    }

    fn mix(a: &[f32], b: &[f32]) -> Vec<f32> {
        a.iter().zip(b).map(|(a, b)| a + b).collect()
    }

    fn tail_db(samples: &[f32], rate: u32) -> f64 {
        let tail = &samples[samples.len() - TAIL_SECS * rate as usize..];
        let energy = tail.iter().map(|&s| f64::from(s) * f64::from(s)).sum::<f64>() / tail.len() as f64;
        10. * (energy + 1e-20).log10()
    }

    fn track(samples: Vec<f32>, rate: u32, start: Instant) -> Track {
        Track { samples, rate, start: Some(start), pad_gaps: false }
    }

    /// 麦克风的底噪，真麦克风再安静也有。
    fn floor(len: usize) -> Vec<f32> {
        noise(len, 99).into_iter().map(|s| s * 1e-4).collect()
    }

    #[test]
    fn removes_echo_of_the_playback() {
        let rate = SAMPLE_RATE;
        let len = SECS * rate as usize;
        let reference: Vec<f32> = noise(len, 1).into_iter().map(|s| s * 0.3).collect();
        let mic = mix(&echo_of(&reference, rate, Duration::from_millis(40)), &floor(len));
        let start = Instant::now();
        let cleaned = cancel(track(mic.clone(), rate, start), track(reference, rate, start));
        assert_eq!(cleaned.len(), mic.len());
        let reduction = tail_db(&mic, rate) - tail_db(&cleaned, rate);
        assert!(reduction >= 30., "echo reduced by only {reduction:.1} dB");
    }

    #[test]
    fn keeps_near_end_when_playback_is_silent() {
        let rate = SAMPLE_RATE;
        let len = SECS * rate as usize;
        let mic = mix(&speech(len, rate), &floor(len));
        let cleaned = process(&mic, &vec![0.; len]).unwrap();
        let loss = tail_db(&mic, rate) - tail_db(&cleaned, rate);
        assert!(loss.abs() <= 1., "near-end changed by {loss:.1} dB");
    }

    #[test]
    fn keeps_near_end_when_playback_does_not_reach_the_mic() {
        let rate = SAMPLE_RATE;
        let len = SECS * rate as usize;
        let reference: Vec<f32> = noise(len, 2).into_iter().map(|s| s * 0.5).collect();
        let mic = mix(&speech(len, rate), &floor(len));
        let start = Instant::now();
        let cleaned = cancel(track(mic.clone(), rate, start), track(reference, rate, start));
        let loss = tail_db(&mic, rate) - tail_db(&cleaned, rate);
        assert!(loss <= 1., "near-end lost {loss:.1} dB");
    }

    #[test]
    fn handles_different_capture_and_render_rates() {
        cancels_echo_across_rates(44_100, 48_000);
    }

    #[test]
    fn handles_rates_that_do_not_split_into_frames() {
        cancels_echo_across_rates(22_050, 48_000);
    }

    /// 麦克风和回采各在自己的采样率上，交回的是 `SAMPLE_RATE` 上消过回声的录音。
    fn cancels_echo_across_rates(mic_rate: u32, reference_rate: u32) {
        let reference: Vec<f32> = noise(SECS * reference_rate as usize, 3).into_iter().map(|s| s * 0.3).collect();
        // 带抗混叠滤波地换到麦克风的采样率上：真麦克风采样前也滤掉了收不下的高频。
        let at_mic_rate = sherpa_onnx::LinearResampler::create(reference_rate as i32, mic_rate as i32)
            .unwrap()
            .resample(&reference, true);
        let mic = mix(&echo_of(&at_mic_rate, mic_rate, Duration::from_millis(40)), &floor(at_mic_rate.len()));
        let start = Instant::now();
        let cleaned = cancel(track(mic.clone(), mic_rate, start), track(reference, reference_rate, start));
        let resampled = resample(&mic, mic_rate);
        assert_eq!(cleaned.len(), resampled.len());
        assert!(cleaned.iter().all(|s| s.is_finite()));
        let reduction = tail_db(&resampled, SAMPLE_RATE) - tail_db(&cleaned, SAMPLE_RATE);
        assert!(reduction >= 20., "echo reduced by only {reduction:.1} dB");
    }

    #[test]
    fn silent_playback_returns_the_recording_untouched() {
        let start = Instant::now();
        let mic = noise(1600, 4);
        let reference = track(vec![0.; 4800], 48_000, start);
        assert_eq!(cancel(track(mic.clone(), SAMPLE_RATE, start), reference), mic);
    }

    #[test]
    fn aligns_reference_by_start_time() {
        let start = Instant::now();
        let mic = track(vec![0.; 10], 1000, start);
        let later = track(vec![1.; 5], 1000, start + Duration::from_millis(3));
        assert_eq!(align(&mic, &later), [0., 0., 0., 1., 1., 1., 1., 1.]);
        let earlier = track(vec![1., 2., 3., 4.], 1000, start);
        let mic = track(vec![0.; 10], 1000, start + Duration::from_millis(2));
        assert_eq!(align(&mic, &earlier), [3., 4.]);
    }
}
