//! 观看端把收到的片拼回帧，按帧号的先后交出去；缺了片就要电脑重发（`Nack`），等太久还没补齐就放弃，丢掉后面的帧
//! 直到下一个关键帧（解码器缺了一帧，后面的都解不对）。

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use runode_protocol::screen::KEYFRAME;

use super::packet::MAX_NACKED;

/// 一帧的片隔这么久没来新的，才当作缺了：片按顺序紧挨着到，乱序也就差一点点。
const REORDER_GRACE: Duration = Duration::from_millis(20);
/// 要重发以后至少等这么久（再加上往返时间）才再要一次。
const NACK_RETRY: Duration = Duration::from_millis(20);
/// 等着交出的那帧卡了这么久（再加上几个往返）还没补齐就放弃。
const GIVE_UP: Duration = Duration::from_millis(150);
/// 最多攒着这么多没交出的帧，再多就是电脑那边一直在丢，全丢掉等关键帧。
const MAX_PENDING: usize = 240;

/// 拼到一半的一帧。
struct Partial {
    chunks: Vec<Option<Vec<u8>>>,
    missing: usize,
    /// 最近来片的时刻、最近一次要重发的时刻。
    last_chunk: Instant,
    nacked: Option<Instant>,
}

impl Partial {
    fn new(count: u16, now: Instant) -> Self {
        Self { chunks: vec![None; usize::from(count)], missing: usize::from(count), last_chunk: now, nacked: None }
    }

    /// 是不是关键帧；第一片还没到时不知道。
    fn keyframe(&self) -> Option<bool> {
        self.chunks.first()?.as_ref().map(|chunk| chunk.first().is_some_and(|flags| flags & KEYFRAME != 0))
    }

    fn missing(&self) -> Vec<u16> {
        self.chunks.iter().enumerate().filter(|(_, chunk)| chunk.is_none()).map(|(index, _)| index as u16).collect()
    }

    fn payload(self) -> Vec<u8> {
        self.chunks.into_iter().flatten().collect::<Vec<_>>().concat()
    }
}

/// `poll` 要做的事。
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Poll {
    /// 要电脑重发的：帧号和缺的片号，片号为空时整帧重发。
    pub(super) nacks: Vec<(u32, Vec<u16>)>,
    /// 放弃了卡住的帧，要向电脑要关键帧。
    pub(super) gave_up: bool,
    /// 放弃卡住的帧以后，后面已经拼齐、能接着交出的帧（从关键帧开始）；有了就不用再要关键帧。
    pub(super) frames: Vec<Vec<u8>>,
}

pub(super) struct Assembler {
    /// 下一个要交出的帧号；等关键帧时是空的。
    next: Option<u32>,
    /// 收到过片的最大帧号。
    top: Option<u32>,
    /// `Pong` 里电脑报的最近发出的帧号和第一次报它的时刻：报的时候那帧的片可能还在路上。
    reported: Option<(u32, Instant)>,
    frames: BTreeMap<u32, Partial>,
    /// 等着交出的那帧从什么时候开始卡住。
    stalled: Option<Instant>,
    /// 交出过的最大帧号：重发晚到的旧片不再收。
    delivered: Option<u32>,
    /// 攒的帧太多、全丢了，下次 `poll` 报放弃，要关键帧。
    overflowed: bool,
    /// 等关键帧时丢掉的差分帧：它们前面没有可能是关键帧的帧，用不上；重发来的片也不收、不再要。
    dropped: BTreeSet<u32>,
    /// 从什么时候开始等关键帧：要来的关键帧整帧丢了时不会再有帧交出，过一阵要再要一次。
    waiting: Option<Instant>,
    /// 收到的片数、第一次发现缺了的片数，给码率调整估丢包。
    pub(super) received: u64,
    pub(super) lost: u64,
}

impl Assembler {
    pub(super) fn new() -> Self {
        Self {
            next: None,
            top: None,
            reported: None,
            frames: BTreeMap::new(),
            stalled: None,
            delivered: None,
            overflowed: false,
            dropped: BTreeSet::new(),
            waiting: None,
            received: 0,
            lost: 0,
        }
    }

    /// 收到一片，返回因此能按顺序交出的帧（`encode_video` 的载荷）。
    pub(super) fn chunk(&mut self, frame: u32, index: u16, count: u16, data: &[u8], now: Instant) -> Vec<Vec<u8>> {
        if self.next.is_some_and(|next| frame < next)
            || self.delivered.is_some_and(|done| frame <= done)
            || self.dropped.contains(&frame)
        {
            return Vec::new();
        }
        self.top = Some(self.top.map_or(frame, |top| top.max(frame)));
        let partial = self.frames.entry(frame).or_insert_with(|| Partial::new(count, now));
        // 整帧要过重发的只占着位置，第一片到了才知道片数。
        if partial.chunks.is_empty() {
            *partial = Partial { nacked: partial.nacked, ..Partial::new(count, now) };
        }
        if partial.chunks.len() != usize::from(count) {
            return Vec::new();
        }
        let Some(slot) = partial.chunks.get_mut(usize::from(index)) else { return Vec::new() };
        if slot.is_none() {
            *slot = Some(data.to_vec());
            partial.missing -= 1;
            self.received += 1;
        }
        partial.last_chunk = now;
        if self.frames.len() > MAX_PENDING {
            self.frames.clear();
            self.next = None;
            self.overflowed = true;
            return Vec::new();
        }
        self.deliver()
    }

    /// 电脑说它最近发到了第 `frame` 帧：整帧都丢了时只有这样才知道。
    pub(super) fn note_top(&mut self, frame: u32, now: Instant) {
        if self.reported.is_none_or(|(reported, _)| frame > reported) {
            self.reported = Some((frame, now));
        }
    }

    /// 按顺序交出拼齐了的帧；等关键帧时从拼齐了的第一个关键帧开始。
    fn deliver(&mut self) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        if self.next.is_none() {
            let Some(&key) = self
                .frames
                .iter()
                .find(|(_, partial)| partial.missing == 0 && partial.keyframe() == Some(true))
                .map(|(frame, _)| frame)
            else {
                return out;
            };
            self.frames = self.frames.split_off(&key);
            self.next = Some(key);
            self.dropped.clear();
            self.waiting = None;
        }
        while let Some(next) = self.next
            && self.frames.get(&next).is_some_and(|partial| partial.missing == 0)
        {
            if let Some(partial) = self.frames.remove(&next) {
                out.push(partial.payload());
            }
            self.delivered = Some(next);
            self.next = Some(next.wrapping_add(1));
            self.stalled = None;
        }
        out
    }

    /// 隔一会儿调一次：列出要重发的，卡太久时放弃。`rtt` 是估计的往返时间。
    pub(super) fn poll(&mut self, now: Instant, rtt: Duration) -> Poll {
        let mut poll = Poll::default();
        if std::mem::take(&mut self.overflowed) {
            poll.gave_up = true;
            return poll;
        }
        let reported =
            self.reported.filter(|(_, at)| now.duration_since(*at) >= REORDER_GRACE + rtt).map(|(frame, _)| frame);
        let Some(top) = self.top.max(reported) else { return poll };
        let first = match self.next {
            Some(next) if next > top => return poll,
            Some(next) => next,
            None => {
                // 从第一个可能是关键帧的帧补起；排在它前面、已知是差分帧的用不上，丢掉。它后面的差分帧留着，关键帧
                // 拼齐了接着交。
                let candidate =
                    self.frames.iter().find(|(_, partial)| partial.keyframe() != Some(false)).map(|(&frame, _)| frame);
                let useless: Vec<u32> = self
                    .frames
                    .range(..candidate.unwrap_or(u32::MAX))
                    .filter(|(_, partial)| partial.keyframe() == Some(false))
                    .map(|(&frame, _)| frame)
                    .collect();
                for frame in useless {
                    self.frames.remove(&frame);
                    self.dropped.insert(frame);
                }
                // 一直等不来（要来的关键帧整帧丢了、片补不上被挤出电脑的记录），再要一次；还在来片时不算。
                let since = *self.waiting.get_or_insert(now);
                let quiet_since = self.frames.values().map(|partial| partial.last_chunk).fold(since, Instant::max);
                if now.duration_since(quiet_since) >= GIVE_UP + rtt * 3 {
                    self.waiting = Some(now);
                    poll.gave_up = true;
                    return poll;
                }
                match candidate {
                    Some(first) => first,
                    None => return poll,
                }
            }
        };
        if let Some(next) = self.next {
            let stalled = *self.stalled.get_or_insert(now);
            // 等着的那帧还在来片（大的关键帧限着速一片片到）就不算卡住。
            let quiet_since = self.frames.get(&next).map_or(stalled, |head| stalled.max(head.last_chunk));
            if now.duration_since(quiet_since) >= GIVE_UP + rtt * 3 {
                self.next = None;
                self.stalled = None;
                self.waiting = Some(now);
                poll.frames = self.deliver();
                poll.gave_up = poll.frames.is_empty();
                return poll;
            }
        }
        let retry = NACK_RETRY + rtt + rtt / 2;
        for frame in first..=top {
            let waiting = self.next.is_some();
            match self.frames.get_mut(&frame) {
                Some(partial) if partial.missing > 0 => {
                    let quiet = now.duration_since(partial.last_chunk) >= REORDER_GRACE;
                    // 要过一次的，重发的片还在陆续到时不再要，不然同一批片排着重发好几遍。
                    let due = partial.nacked.is_none_or(|at| {
                        now.duration_since(at) >= retry && now.duration_since(partial.last_chunk) >= retry
                    });
                    if (frame < top || quiet) && due {
                        let mut missing = partial.missing();
                        if partial.nacked.is_none() {
                            self.lost += missing.len() as u64;
                        }
                        missing.truncate(MAX_NACKED);
                        partial.nacked = Some(now);
                        poll.nacks.push((frame, missing));
                    }
                }
                Some(_) => {}
                // 一片都没收到：按顺序等着它、或等关键帧时它排在第一个可能是关键帧的帧后面，要整帧重发。
                None if !self.dropped.contains(&frame) && (waiting || frame > first) => {
                    let partial = self.frames.entry(frame).or_insert_with(|| Partial::new(1, now));
                    partial.chunks.clear();
                    partial.missing = usize::MAX;
                    partial.nacked = Some(now);
                    self.lost += 1;
                    poll.nacks.push((frame, Vec::new()));
                }
                None => {}
            }
        }
        poll
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(keyframe: bool, len: usize) -> Vec<u8> {
        let mut out = vec![u8::from(keyframe)];
        out.extend((1..len).map(|byte| byte as u8));
        out
    }

    /// 把 `payload` 切成 `size` 字节一片。
    fn chunks(payload: &[u8], size: usize) -> Vec<Vec<u8>> {
        payload.chunks(size).map(<[u8]>::to_vec).collect()
    }

    fn feed(assembler: &mut Assembler, frame: u32, parts: &[Vec<u8>], skip: &[usize], now: Instant) -> Vec<Vec<u8>> {
        let count = parts.len() as u16;
        let mut out = Vec::new();
        for (index, part) in parts.iter().enumerate() {
            if !skip.contains(&index) {
                out.extend(assembler.chunk(frame, index as u16, count, part, now));
            }
        }
        out
    }

    #[test]
    fn delivers_frames_in_order_from_the_first_keyframe() {
        let now = Instant::now();
        let mut assembler = Assembler::new();
        let delta = payload(false, 50);
        assert!(feed(&mut assembler, 0, &chunks(&delta, 20), &[], now).is_empty(), "no keyframe yet");
        let key = payload(true, 70);
        let next = payload(false, 30);
        // 后一帧先到，等前一帧拼齐了一起交出。
        assert!(feed(&mut assembler, 2, &chunks(&next, 20), &[], now).is_empty());
        assert_eq!(feed(&mut assembler, 1, &chunks(&key, 20), &[], now), vec![key, next]);
    }

    #[test]
    fn asks_for_missing_chunks_and_delivers_once_repaired() {
        let start = Instant::now();
        let mut assembler = Assembler::new();
        let key = payload(true, 100);
        let parts = chunks(&key, 10);
        assert!(feed(&mut assembler, 0, &parts, &[3, 7], start).is_empty());
        assert_eq!(assembler.poll(start, Duration::ZERO), Poll::default(), "chunks may still be on their way");
        let later = start + REORDER_GRACE;
        assert_eq!(assembler.poll(later, Duration::ZERO).nacks, vec![(0, vec![3, 7])]);
        assert!(assembler.poll(later, Duration::ZERO).nacks.is_empty(), "not again right away");
        assert!(assembler.chunk(0, 3, 10, &parts[3], later).is_empty());
        assert_eq!(assembler.chunk(0, 7, 10, &parts[7], later), vec![key]);
        assert_eq!(assembler.lost, 2);
    }

    /// 整帧丢了：后面的帧到了，或者电脑报了它发到哪儿，就要整帧重发。
    #[test]
    fn asks_for_whole_frames_that_never_arrived() {
        let now = Instant::now();
        let mut assembler = Assembler::new();
        feed(&mut assembler, 0, &chunks(&payload(true, 10), 10), &[], now);
        assembler.note_top(2, now);
        assert!(assembler.poll(now, Duration::ZERO).nacks.is_empty(), "the frames may still be on their way");
        let poll = assembler.poll(now + REORDER_GRACE, Duration::ZERO);
        assert_eq!(poll.nacks, vec![(1, Vec::new()), (2, Vec::new())]);
        // 重发的片到了，拼齐以后照样按顺序交出。
        let one = payload(false, 25);
        let two = payload(false, 5);
        assert!(feed(&mut assembler, 2, &chunks(&two, 10), &[], now).is_empty());
        assert_eq!(feed(&mut assembler, 1, &chunks(&one, 10), &[], now), vec![one, two]);
    }

    /// 补不齐就放弃，丢掉后面的非关键帧，下一个关键帧拼齐了再接着交。
    #[test]
    fn gives_up_and_waits_for_the_next_keyframe() {
        let start = Instant::now();
        let mut assembler = Assembler::new();
        feed(&mut assembler, 0, &chunks(&payload(true, 10), 10), &[], start);
        feed(&mut assembler, 1, &chunks(&payload(false, 30), 10), &[1], start);
        feed(&mut assembler, 2, &chunks(&payload(false, 10), 10), &[], start);
        assert!(!assembler.poll(start, Duration::ZERO).gave_up);
        assert!(assembler.poll(start + GIVE_UP, Duration::ZERO).gave_up);
        let delta = payload(false, 10);
        assert!(feed(&mut assembler, 3, &chunks(&delta, 10), &[], start + GIVE_UP).is_empty());
        let key = payload(true, 10);
        let after = payload(false, 10);
        assert_eq!(feed(&mut assembler, 4, &chunks(&key, 10), &[], start + GIVE_UP), vec![key]);
        assert_eq!(feed(&mut assembler, 5, &chunks(&after, 10), &[], start + GIVE_UP), vec![after]);
    }

    /// 放弃时后面已经有拼齐的关键帧，当场从它接着交，不再要关键帧。
    #[test]
    fn delivers_a_ready_keyframe_when_giving_up() {
        let start = Instant::now();
        let mut assembler = Assembler::new();
        feed(&mut assembler, 0, &chunks(&payload(true, 10), 10), &[], start);
        feed(&mut assembler, 1, &chunks(&payload(false, 30), 10), &[1], start);
        let key = payload(true, 10);
        assert!(feed(&mut assembler, 2, &chunks(&key, 10), &[], start).is_empty());
        assert!(assembler.poll(start, Duration::ZERO).frames.is_empty());
        let poll = assembler.poll(start + GIVE_UP, Duration::ZERO);
        assert_eq!(poll.frames, vec![key]);
        assert!(!poll.gave_up);
    }

    /// 要过重发以后，重发的片还在陆续到就不再要，一片都不来了才再要。
    #[test]
    fn asks_again_only_when_the_resent_chunks_stop() {
        let start = Instant::now();
        let mut assembler = Assembler::new();
        let parts = chunks(&payload(true, 100), 10);
        feed(&mut assembler, 0, &parts, &[3, 5, 7], start);
        assert_eq!(assembler.poll(start + REORDER_GRACE, Duration::ZERO).nacks, vec![(0, vec![3, 5, 7])]);
        let resent = start + REORDER_GRACE + NACK_RETRY;
        assert!(assembler.chunk(0, 3, 10, &parts[3], resent).is_empty());
        assert!(assembler.poll(resent, Duration::ZERO).nacks.is_empty(), "the resent chunks are arriving");
        assert_eq!(assembler.poll(resent + NACK_RETRY, Duration::ZERO).nacks, vec![(0, vec![5, 7])]);
    }

    /// 放弃以后，重发晚到的已经交出过的帧不再收，不会倒回去。
    #[test]
    fn ignores_delivered_frames_after_giving_up() {
        let start = Instant::now();
        let mut assembler = Assembler::new();
        let key = payload(true, 10);
        feed(&mut assembler, 0, &chunks(&key, 10), &[], start);
        feed(&mut assembler, 1, &chunks(&payload(false, 20), 10), &[1], start);
        assert!(!assembler.poll(start, Duration::ZERO).gave_up);
        assert!(assembler.poll(start + GIVE_UP, Duration::ZERO).gave_up);
        assert!(feed(&mut assembler, 0, &chunks(&key, 10), &[], start + GIVE_UP).is_empty());
    }

    /// 等着的那帧一直在来片时不算卡住。
    #[test]
    fn a_slowly_arriving_frame_is_not_given_up() {
        let start = Instant::now();
        let mut assembler = Assembler::new();
        feed(&mut assembler, 0, &chunks(&payload(true, 10), 10), &[], start);
        let big = chunks(&payload(false, 100), 10);
        let mut now = start;
        for (index, part) in big.iter().enumerate().take(9) {
            assembler.chunk(1, index as u16, 10, part, now);
            assert!(!assembler.poll(now, Duration::ZERO).gave_up);
            now += GIVE_UP / 2;
        }
        assert_eq!(assembler.chunk(1, 9, 10, &big[9], now).len(), 1);
    }

    /// 攒的帧太多、全丢了以后报放弃，好去要关键帧。
    #[test]
    fn overflowing_reports_a_gap() {
        let now = Instant::now();
        let mut assembler = Assembler::new();
        for frame in 0..=MAX_PENDING as u32 {
            assembler.chunk(frame, 0, 2, &[0], now);
        }
        assert!(assembler.poll(now, Duration::ZERO).gave_up);
    }

    /// 要来的关键帧整帧丢了，等一阵再要一次，不会一直干等。
    #[test]
    fn asks_again_when_the_keyframe_never_comes() {
        let start = Instant::now();
        let mut assembler = Assembler::new();
        assembler.chunk(5, 0, 1, &[0], start);
        assert!(!assembler.poll(start, Duration::ZERO).gave_up);
        assert!(assembler.poll(start + GIVE_UP, Duration::ZERO).gave_up);
        assert!(!assembler.poll(start + GIVE_UP, Duration::ZERO).gave_up, "not again right away");
        assert!(assembler.poll(start + GIVE_UP * 2, Duration::ZERO).gave_up);
    }

    /// 等关键帧时，关键帧前面的差分帧丢掉、不再要；关键帧后面先到的差分帧留着，关键帧拼齐了一起交出。
    #[test]
    fn keeps_deltas_after_the_awaited_keyframe() {
        let now = Instant::now();
        let mut assembler = Assembler::new();
        let stale = payload(false, 10);
        feed(&mut assembler, 1, &chunks(&stale, 10), &[], now);
        let key = chunks(&payload(true, 30), 10);
        assembler.chunk(2, 0, 3, &key[0], now);
        let after = payload(false, 10);
        feed(&mut assembler, 3, &chunks(&after, 10), &[], now);
        let poll = assembler.poll(now + REORDER_GRACE, Duration::ZERO);
        assert_eq!(poll.nacks, vec![(2, vec![1, 2])], "only the keyframe needs repairs");
        assert!(feed(&mut assembler, 1, &chunks(&stale, 10), &[], now).is_empty(), "dropped deltas stay dropped");
        assembler.chunk(2, 1, 3, &key[1], now);
        let out = assembler.chunk(2, 2, 3, &key[2], now);
        assert_eq!(out, vec![key.concat(), after]);
    }

    /// 重复的片和交出过的帧的片不算数。
    #[test]
    fn ignores_duplicates_and_old_frames() {
        let now = Instant::now();
        let mut assembler = Assembler::new();
        let key = payload(true, 20);
        let parts = chunks(&key, 10);
        assert!(assembler.chunk(0, 0, 2, &parts[0], now).is_empty());
        assert!(assembler.chunk(0, 0, 2, &parts[0], now).is_empty());
        assert_eq!(assembler.chunk(0, 1, 2, &parts[1], now), vec![key]);
        assert!(assembler.chunk(0, 1, 2, &parts[1], now).is_empty());
        assert_eq!(assembler.received, 2);
    }
}
