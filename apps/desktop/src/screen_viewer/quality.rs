//! 远程桌面的清晰度：几档固定的（`Level`），和按网络自动挑档、调码率（`Network` 量，`Adapter` 调）。
//!
//! 网络好坏看画面在路上排了多久队：每帧到的时刻减去它在远端的时间戳。两台机器的时钟不同，这个差本身没有
//! 意义，只比它高出最近一段时间里的最小值多少；高出得多，说明发出去的超过了路上过得去的，堆在了 ssh 和
//! TCP 的缓冲里，或者走 UDP 时丢了包在等重发。走 UDP 时另看丢包：丢得多也算堵了。码率随时可以改，不用重开
//! 采集；换档要改画面尺寸、重开采集，所以要持续一阵才换。

use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// 一档清晰度，从低到高排。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Smooth,
    Standard,
    High,
    Ultra,
    Original,
}

impl Level {
    /// 菜单里的次序。
    pub const MENU: [Level; 5] = [Level::Original, Level::Ultra, Level::High, Level::Standard, Level::Smooth];

    /// 画面短边最多多少像素；不限时为空：原画照屏幕原样，超清照页面大小。
    pub fn lines(self) -> Option<u32> {
        match self {
            Level::Original | Level::Ultra => None,
            Level::High => Some(1080),
            Level::Standard => Some(720),
            Level::Smooth => Some(480),
        }
    }

    /// 码率上限，单位 bit/s。
    pub fn max_bitrate(self) -> u32 {
        match self {
            Level::Original => 30_000_000,
            Level::Ultra => 20_000_000,
            Level::High => 8_000_000,
            Level::Standard => 4_000_000,
            Level::Smooth => 1_500_000,
        }
    }

    /// 码率 `bitrate` 下自动挑的档：撑得起（`NEED_SHARE`）这一档在现在的页面里要的码率（`needs`）的最高一档。
    /// 页面小、几档的画面一样大时它们要的码率也一样，网络撑得住就是超清，不因为画面小用不了多少码率就往低了叫。
    fn for_bitrate(bitrate: u32, needs: &Needs) -> Self {
        [(Level::Ultra, needs.ultra), (Level::High, needs.high), (Level::Standard, needs.standard)]
            .into_iter()
            .find(|(_, need)| f64::from(bitrate) >= f64::from(*need) * NEED_SHARE)
            .map_or(Level::Smooth, |(level, _)| level)
    }
}

/// 码率到了一档要的这么多就用这一档：画面里大多是不动的，给不足它要的那么多也看得清。
const NEED_SHARE: f64 = 0.5;

/// 自动挑的几档在现在的页面里各要多少码率（按画面尺寸算，不超过各档的上限），单位 bit/s。最高一档要的也是自动
/// 调码率的上限：画面再大也用不上更多。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Needs {
    pub ultra: u32,
    pub high: u32,
    pub standard: u32,
}

/// 排队时差的基准取最近这么久里的最小值：太短的话一直堵着时基准跟着涨上去，看着就像不堵了。
const BASELINE_WINDOW: Duration = Duration::from_secs(30);
/// 基准按这么长一格记最小值。
const BASELINE_SLOT: Duration = Duration::from_secs(1);
/// 远端开始采集后这么久里只算大小，不比时差：头一帧关键帧和重建编码器把后面的帧堵一阵，刚换过显示模式时
/// 还要更久，都不是网络堵。
const WARMUP: Duration = Duration::from_secs(3);

/// 一轮里网络的样子。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sample {
    /// 画面平均在路上多排了多久队；这一轮没有能比的帧时为空。
    pub queuing: Option<Duration>,
    /// 这一轮到了几帧能比时差的画面。
    pub frames: u32,
    /// 这一轮实际收到的码率，单位 bit/s。
    pub throughput: u32,
    /// 画面走 UDP 时这一轮丢了的片占的比例，不走时是 0。
    pub loss: f64,
}

/// 在读连接的线程上记下每帧到的时刻和大小，界面隔一阵取走一轮（`sample`）。
pub struct Network {
    epoch: Instant,
    started: Instant,
    /// 每格（`BASELINE_SLOT`）里最小的时差（微秒）和这一格开始的时刻。
    floors: VecDeque<(Instant, i64)>,
    /// 这一轮时差的和；固定档时一直没人取走，用 i128 才不会溢出。
    sum: i128,
    frames: u32,
    bytes: u64,
    since: Instant,
    /// UDP 上收到的和丢了的片数：上一轮取样时的累计和现在的累计。
    counted: (u64, u64),
    counts: (u64, u64),
}

impl Network {
    pub fn new(now: Instant) -> Self {
        Self {
            epoch: now,
            started: now,
            floors: VecDeque::new(),
            sum: 0,
            frames: 0,
            bytes: 0,
            since: now,
            counted: (0, 0),
            counts: (0, 0),
        }
    }

    /// 远端重新开始采集了：时间戳换了起点，之前的基准不能再比。
    pub fn reset(&mut self, now: Instant) {
        *self = Self { epoch: self.epoch, counted: self.counts, counts: self.counts, ..Self::new(now) };
    }

    /// 画面走 UDP 时到现在一共收到、丢了多少片。
    pub fn datagrams(&mut self, received: u64, lost: u64) {
        self.counts = (received, lost);
    }

    /// 到了一帧。关键帧只算大小，不比时差：它们大，本来就到得晚；远端屏幕静止时补编的关键帧时间戳也不是
    /// 采集的时刻。
    pub fn frame(&mut self, now: Instant, pts_us: i64, bytes: usize, keyframe: bool) {
        self.bytes += bytes as u64;
        if keyframe || now.duration_since(self.started) < WARMUP {
            return;
        }
        let offset = now.duration_since(self.epoch).as_micros() as i64 - pts_us;
        match self.floors.back_mut() {
            Some((start, floor)) if now.duration_since(*start) < BASELINE_SLOT => *floor = (*floor).min(offset),
            _ => self.floors.push_back((now, offset)),
        }
        while self.floors.front().is_some_and(|(start, _)| now.duration_since(*start) > BASELINE_WINDOW) {
            self.floors.pop_front();
        }
        self.sum += i128::from(offset);
        self.frames += 1;
    }

    /// 取走从上回取到现在的一轮。
    pub fn sample(&mut self, now: Instant) -> Sample {
        let elapsed = now.duration_since(self.since).as_secs_f64();
        let throughput =
            if elapsed > 0. { (self.bytes as f64 * 8. / elapsed).min(f64::from(u32::MAX)) as u32 } else { 0 };
        let baseline = self.floors.iter().map(|(_, floor)| *floor).min();
        let queuing = baseline.filter(|_| self.frames > 0).map(|baseline| {
            let mean = (self.sum / i128::from(self.frames)) as i64;
            Duration::from_micros((mean - baseline).max(0) as u64)
        });
        let received = self.counts.0.saturating_sub(self.counted.0);
        let lost = self.counts.1.saturating_sub(self.counted.1);
        let loss = if lost == 0 { 0. } else { lost as f64 / (received + lost) as f64 };
        self.counted = self.counts;
        let sample = Sample { queuing, frames: self.frames, throughput, loss };
        self.sum = 0;
        self.frames = 0;
        self.bytes = 0;
        self.since = now;
        sample
    }
}

/// 自动档开始时的码率和档：先不往高了要，网络好时几秒内升上去。
const START_BITRATE: u32 = 6_000_000;
/// 自动调时码率最低到这么多。
const MIN_BITRATE: u32 = 500_000;
/// 排队超过这么久算堵了；连着两轮才降：一帧大的关键帧、远端编码器忙一下堵一阵，不是网络堵。
const CONGESTED: Duration = Duration::from_millis(120);
/// 走 UDP 时一轮里丢了这么大比例的片也算堵了：Wi-Fi 上零星丢一点是常事，补得回来。
const LOSSY: f64 = 0.05;
/// 排队不到这么久算通畅，可以往上加。
const CLEAR: Duration = Duration::from_millis(40);
/// 堵了时码率乘这么多，最多降到一半；收到的码率到了要的 `LOADED` 那么多、又比这低时按它的 `THROUGHPUT_SHARE` 降。
/// 收到的少得多时说明画面没怎么动、发得少，收到多少说明不了路上过得去多少。
const CUT: f64 = 0.75;
const THROUGHPUT_SHARE: f64 = 0.85;
const LOADED: f64 = 0.5;
/// 通畅时码率乘这么多。
const GROW: f64 = 1.1;
/// 降过以后至少隔这么久再降，等缓冲里堆着的排出去。
const CUT_INTERVAL: Duration = Duration::from_secs(1);
/// 降过以后这么久里不往上加。
const HOLD_AFTER_CUT: Duration = Duration::from_secs(3);
/// 一轮里至少到这么多帧才往上加：屏幕静止时没有帧，看不出网络能不能过更多。
const MIN_FRAMES: u32 = 5;
/// 码率指向更低的档持续这么久才降档。
const DOWN_DELAY: Duration = Duration::from_secs(4);
/// 码率指向更高的档持续这么久才升档。
const UP_DELAY: Duration = Duration::from_secs(8);
/// 换过档以后至少这么久不再换：换档要重开采集，码率随时能调，画面尺寸不跟着来回切。
const LEVEL_HOLD: Duration = Duration::from_secs(15);

/// 自动档：按每轮的 `Sample` 估网络撑得住多少码率（`allowed`），要的码率是它和现在的画面用得上的（`Needs::ultra`）
/// 里小的那个（`bitrate`）；码率稳定在哪一档的范围里就换到哪一档。两个分开记：页面缩小时用不上那么多，网络撑得住
/// 的还记着，放大后马上照它要，不用等画面动起来再慢慢涨。
pub struct Adapter {
    allowed: u32,
    bitrate: u32,
    level: Level,
    /// 码率指向的另一档和从什么时候起一直指向它。
    pending: Option<(Level, Instant)>,
    /// 上回换档的时候。
    switched: Option<Instant>,
    last_cut: Option<Instant>,
    last_queuing: Duration,
    /// 连着几轮堵了。
    congested: u32,
}

impl Default for Adapter {
    fn default() -> Self {
        Self {
            allowed: START_BITRATE,
            bitrate: START_BITRATE,
            level: Level::High,
            pending: None,
            switched: None,
            last_cut: None,
            last_queuing: Duration::ZERO,
            congested: 0,
        }
    }
}

impl Adapter {
    /// 从码率 `bitrate` 和档 `level`（原画按超清算）接着调。
    pub fn starting_at(bitrate: u32, level: Level) -> Self {
        let bitrate = bitrate.max(MIN_BITRATE);
        Self { allowed: bitrate, bitrate, level: level.min(Level::Ultra), ..Self::default() }
    }

    /// 现在要的码率，单位 bit/s。
    pub fn bitrate(&self) -> u32 {
        self.bitrate
    }

    /// 现在用的档。
    pub fn level(&self) -> Level {
        self.level
    }

    /// 按这一轮的网络调码率和档，码率不超过最高一档要的（`Needs::ultra`）；返回变了没有。
    pub fn update(&mut self, sample: &Sample, needs: &Needs, now: Instant) -> bool {
        let before = (self.bitrate, self.level);
        let ceiling = needs.ultra.max(MIN_BITRATE);
        if let Some(queuing) = sample.queuing {
            self.congested = if queuing > CONGESTED || sample.loss >= LOSSY { self.congested + 1 } else { 0 };
            let since_cut = self.last_cut.map(|at| now.duration_since(at));
            // 排队已经在变短，说明上回降的起作用了，等它排完。
            let draining = queuing < self.last_queuing.mul_f64(0.9);
            if self.congested >= 2 && !draining && since_cut.is_none_or(|since| since >= CUT_INTERVAL) {
                // 按实际在发的码率降，不按估的：页面小时估的可能比发的高得多。
                let by_rate = f64::from(self.bitrate) * CUT;
                let throughput = f64::from(sample.throughput);
                let loaded = throughput >= f64::from(self.bitrate) * LOADED;
                let by_throughput = if loaded { throughput * THROUGHPUT_SHARE } else { by_rate };
                let cut = by_rate.min(by_throughput).max(f64::from(self.bitrate) / 2.);
                self.allowed = (cut as u32).max(MIN_BITRATE);
                self.last_cut = Some(now);
            } else if queuing < CLEAR
                && self.congested == 0
                && sample.frames >= MIN_FRAMES
                && since_cut.is_none_or(|since| since >= HOLD_AFTER_CUT)
                // 画面用不上估的那么多时，发出去的比估的少，通畅也说明不了网络撑得住更多。
                && self.allowed < ceiling
            {
                self.allowed = ((f64::from(self.allowed) * GROW) as u32).min(Level::Ultra.max_bitrate());
            }
            self.last_queuing = queuing;
        }
        self.bitrate = self.allowed.min(ceiling);
        let target = Level::for_bitrate(self.bitrate, needs);
        if target == self.level {
            self.pending = None;
        } else {
            let since = match self.pending {
                Some((level, since)) if level == target => since,
                _ => self.pending.insert((target, now)).1,
            };
            let wait = if target < self.level { DOWN_DELAY } else { UP_DELAY };
            let held = self.switched.is_none_or(|at| now.duration_since(at) >= LEVEL_HOLD);
            if now.duration_since(since) >= wait && held {
                self.level = target;
                self.pending = None;
                self.switched = Some(now);
            }
        }
        (self.bitrate, self.level) != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TICK: Duration = Duration::from_millis(500);

    /// 隔 `TICK` 到一帧、每帧在路上多排 `extra` 毫秒队的一轮。
    fn round(network: &mut Network, now: &mut Instant, pts: &mut i64, extras: &[i64]) -> Sample {
        let step = TICK / extras.len() as u32;
        for &extra in extras {
            *now += step;
            *pts += step.as_micros() as i64;
            network.frame(*now, *pts - extra * 1000, 10_000, false);
        }
        network.sample(*now)
    }

    #[test]
    fn queuing_is_measured_against_the_lowest_offset() {
        let start = Instant::now();
        let mut network = Network::new(start);
        let (mut now, mut pts) = (start, 1_000_000_000);
        // 刚开始采集时堵着的不算。
        for _ in 0..5 {
            let warmup = round(&mut network, &mut now, &mut pts, &[900, 600]);
            assert_eq!((warmup.queuing, warmup.frames), (None, 0));
        }
        round(&mut network, &mut now, &mut pts, &[300]);
        let calm = round(&mut network, &mut now, &mut pts, &[0, 0, 0, 0]);
        assert_eq!(calm.queuing, Some(Duration::ZERO));
        assert_eq!(calm.frames, 4);
        assert_eq!(calm.throughput, 640_000);
        // 到的时刻比时间戳越拖越后：后到的帧在路上排了队。
        let busy = round(&mut network, &mut now, &mut pts, &[100, 200, 300, 400]);
        assert_eq!(busy.queuing, Some(Duration::from_millis(250)));
        // 关键帧不比时差。
        network.frame(now, 0, 10_000, true);
        assert_eq!(network.sample(now).queuing, None);
    }

    /// 整块大页面里几档各要的码率。
    const FULL: Needs = Needs { ultra: 16_000_000, high: 8_000_000, standard: 4_000_000 };

    #[test]
    fn congestion_cuts_the_bitrate_and_then_the_level() {
        let start = Instant::now();
        let mut adapter = Adapter::default();
        let jammed = Sample { queuing: Some(Duration::from_millis(600)), frames: 30, throughput: 4_000_000, loss: 0. };
        // 一轮堵一下不降，堵得再久也一样。
        assert!(!adapter.update(&jammed, &FULL, start));
        // 连着两轮堵了，降到收到的码率以下，但一次最多降一半。
        assert!(adapter.update(&jammed, &FULL, start + TICK));
        assert_eq!(adapter.bitrate(), 3_400_000);
        assert_eq!(adapter.level(), Level::High);
        // 一直指向更低的档，过了 `DOWN_DELAY` 才换。
        let calm = Sample { queuing: Some(Duration::from_millis(80)), frames: 30, throughput: 2_000_000, loss: 0. };
        adapter.update(&calm, &FULL, start + TICK * 2);
        assert_eq!(adapter.level(), Level::High);
        let switched = start + TICK + DOWN_DELAY;
        adapter.update(&calm, &FULL, switched);
        assert_eq!(adapter.level(), Level::Standard);
        // 换过档以后，码率涨回去了也要等 `LEVEL_HOLD` 才升回来。
        let clear = Sample { queuing: Some(Duration::from_millis(5)), frames: 30, throughput: 6_000_000, loss: 0. };
        let mut now = switched;
        while now < switched + LEVEL_HOLD - TICK {
            now += TICK;
            adapter.update(&clear, &FULL, now);
            assert_eq!(adapter.level(), Level::Standard);
        }
        adapter.update(&clear, &FULL, switched + LEVEL_HOLD);
        assert!(adapter.level() > Level::Standard);
    }

    /// 走 UDP 时丢包多也算堵了，同样连着两轮才降。
    #[test]
    fn heavy_loss_counts_as_congestion() {
        let start = Instant::now();
        let mut adapter = Adapter::default();
        let lossy = Sample { queuing: Some(Duration::from_millis(10)), frames: 30, throughput: 6_000_000, loss: 0.2 };
        assert!(!adapter.update(&lossy, &FULL, start));
        assert!(adapter.update(&lossy, &FULL, start + TICK));
        assert!(adapter.bitrate() < START_BITRATE);
        // 零星丢一点不算。
        let mut calm = Adapter::default();
        let light = Sample { loss: 0.01, ..lossy };
        calm.update(&light, &FULL, start);
        calm.update(&light, &FULL, start + TICK);
        assert!(calm.bitrate() >= START_BITRATE);
    }

    /// UDP 的丢包率按两次取样之间的片数算。
    #[test]
    fn loss_is_measured_per_round() {
        let start = Instant::now();
        let mut network = Network::new(start);
        network.datagrams(90, 10);
        assert!((network.sample(start + TICK).loss - 0.1).abs() < 1e-9);
        network.datagrams(190, 10);
        assert_eq!(network.sample(start + TICK * 2).loss, 0.);
    }

    /// 画面没怎么动、收到的很少时堵了，按比例降，不照收到的那点码率一下砍到底。
    #[test]
    fn congestion_with_little_traffic_cuts_gently() {
        let start = Instant::now();
        let mut adapter = Adapter::default();
        let idle_jam = Sample { queuing: Some(Duration::from_millis(400)), frames: 3, throughput: 100_000, loss: 0. };
        adapter.update(&idle_jam, &FULL, start);
        adapter.update(&idle_jam, &FULL, start + TICK);
        assert_eq!(adapter.bitrate(), 4_500_000);
    }

    #[test]
    fn a_clear_network_grows_slowly_up_to_the_ceiling() {
        let start = Instant::now();
        let mut adapter = Adapter::default();
        let clear = Sample { queuing: Some(Duration::from_millis(5)), frames: 30, throughput: 6_000_000, loss: 0. };
        let needs = Needs { ultra: 12_000_000, ..FULL };
        let mut now = start;
        for _ in 0..40 {
            now += TICK;
            adapter.update(&clear, &needs, now);
        }
        assert_eq!(adapter.bitrate(), 12_000_000);
        assert_eq!(adapter.level(), Level::Ultra);
        // 屏幕静止、没几帧时不往上加。
        let mut idle = Adapter::default();
        idle.update(&Sample { frames: 1, ..clear }, &needs, start + TICK);
        assert_eq!(idle.bitrate(), START_BITRATE);
    }

    /// 页面小、几档的画面一样大时，网络撑得住就是超清，码率封顶在它要的那么多。
    #[test]
    fn a_small_page_is_ultra_when_the_network_keeps_up() {
        let start = Instant::now();
        let mut adapter = Adapter::default();
        let small = Needs { ultra: 2_000_000, high: 2_000_000, standard: 2_000_000 };
        let clear = Sample { queuing: Some(Duration::from_millis(5)), frames: 30, throughput: 2_000_000, loss: 0. };
        adapter.update(&clear, &small, start);
        assert_eq!(adapter.bitrate(), 2_000_000);
        adapter.update(&clear, &small, start + UP_DELAY);
        assert_eq!(adapter.level(), Level::Ultra);
        // 放大后马上照网络撑得住的要，屏幕静止、没有帧也一样。
        let idle = Sample { queuing: None, frames: 0, throughput: 0, loss: 0. };
        adapter.update(&idle, &FULL, start + UP_DELAY + TICK);
        assert_eq!(adapter.bitrate(), START_BITRATE);
    }
}
