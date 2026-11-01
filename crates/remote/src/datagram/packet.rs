//! UDP 包的格式和加密。一个包是 `u64 BE 计数 | 密文和 16 字节的认证标签`，用 ChaCha20-Poly1305 加密，nonce 是
//! `u8 方向（电脑发的 0，观看端发的 1）| 3 字节 0 | 计数`；每一头发的包计数从 0 往上走，同一个密钥下 nonce 不重复。
//! 明文是 `u8 类型 | 内容`：
//!
//! - 1 `Probe`（电脑发）：没有内容，往观看端的地址打洞。
//! - 2 `Ping`（观看端发）：`u64 BE 发出的时刻（微秒）| u8 收到过电脑的包`；也是保活，电脑凭它认出观看端的地址。
//! - 3 `Pong`（电脑发）：`u64 BE 原样带回的时刻 | u32 BE 最近发出的帧号`，没发过帧时是 `u32::MAX`。
//! - 4 `Video`（电脑发）：`u32 BE 帧号 | u16 BE 片号 | u16 BE 片数 | 这一片`。一帧是 `runode_protocol::screen`
//!   的 `encode_video` 载荷，切成 `CHUNK` 字节一片；帧号从 0 起逐帧加一。
//! - 5 `Nack`（观看端发）：`u32 BE 帧号 | u16 BE 个数 | 每个缺的片号 u16 BE`，个数为 0 时整帧重发。

use std::sync::atomic::{AtomicU64, Ordering};

use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};

/// 一个包最长这么多字节：经 VPN（Tailscale 的 MTU 是 1280）和 IPv6 的最小 MTU 也不分片。
pub(super) const MAX_PACKET: usize = 1200;
const COUNTER: usize = 8;
const TAG: usize = 16;
const VIDEO_HEADER: usize = 1 + 4 + 2 + 2;
/// 一片画面最多这么多字节。
pub(super) const CHUNK: usize = MAX_PACKET - COUNTER - TAG - VIDEO_HEADER;
/// 一个 `Nack` 最多列这么多片，装得进一个包。
pub(super) const MAX_NACKED: usize = (MAX_PACKET - COUNTER - TAG - 7) / 2;

/// 谁发的包，两个方向的 nonce 不重叠。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Direction {
    FromHost = 0,
    FromViewer = 1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Packet<'a> {
    Probe,
    Ping { sent_us: u64, heard: bool },
    Pong { sent_us: u64, last_frame: Option<u32> },
    Video { frame: u32, index: u16, count: u16, data: &'a [u8] },
    Nack { frame: u32, missing: Vec<u16> },
}

impl Packet<'_> {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Packet::Probe => out.push(1),
            Packet::Ping { sent_us, heard } => {
                out.push(2);
                out.extend(sent_us.to_be_bytes());
                out.push(u8::from(*heard));
            }
            Packet::Pong { sent_us, last_frame } => {
                out.push(3);
                out.extend(sent_us.to_be_bytes());
                out.extend(last_frame.unwrap_or(u32::MAX).to_be_bytes());
            }
            Packet::Video { frame, index, count, data } => {
                out.push(4);
                out.extend(frame.to_be_bytes());
                out.extend(index.to_be_bytes());
                out.extend(count.to_be_bytes());
                out.extend_from_slice(data);
            }
            Packet::Nack { frame, missing } => {
                out.push(5);
                out.extend(frame.to_be_bytes());
                out.extend((missing.len() as u16).to_be_bytes());
                for index in missing {
                    out.extend(index.to_be_bytes());
                }
            }
        }
    }

    fn decode(plain: &[u8]) -> Option<Packet<'_>> {
        let (&kind, rest) = plain.split_first()?;
        let u16_at = |bytes: &[u8], at: usize| Some(u16::from_be_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
        let u32_at = |bytes: &[u8], at: usize| Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
        let u64_at = |bytes: &[u8], at: usize| Some(u64::from_be_bytes(bytes.get(at..at + 8)?.try_into().ok()?));
        Some(match kind {
            1 => Packet::Probe,
            2 => Packet::Ping { sent_us: u64_at(rest, 0)?, heard: *rest.get(8)? != 0 },
            3 => {
                let last = u32_at(rest, 8)?;
                Packet::Pong { sent_us: u64_at(rest, 0)?, last_frame: (last != u32::MAX).then_some(last) }
            }
            4 => {
                let (frame, index, count) = (u32_at(rest, 0)?, u16_at(rest, 4)?, u16_at(rest, 6)?);
                if count == 0 || index >= count {
                    return None;
                }
                Packet::Video { frame, index, count, data: &rest[8..] }
            }
            5 => {
                let (frame, len) = (u32_at(rest, 0)?, u16_at(rest, 4)?);
                let missing = (0..usize::from(len)).map(|at| u16_at(rest, 6 + at * 2)).collect::<Option<_>>()?;
                Packet::Nack { frame, missing }
            }
            _ => return None,
        })
    }
}

fn nonce(direction: Direction, counter: u64) -> Nonce {
    let mut bytes = [0u8; NONCE_LEN];
    bytes[0] = direction as u8;
    bytes[4..].copy_from_slice(&counter.to_be_bytes());
    Nonce::assume_unique_for_key(bytes)
}

fn key(secret: &[u8; 32]) -> LessSafeKey {
    // 密钥长度对得上，建不成只能是 ring 坏了。
    LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, secret).expect("a ChaCha20-Poly1305 key"))
}

/// 加密一头发出的包；可以在几个线程上同时用。
pub(super) struct Sealer {
    key: LessSafeKey,
    direction: Direction,
    counter: AtomicU64,
}

impl Sealer {
    pub(super) fn new(secret: &[u8; 32], direction: Direction) -> Self {
        Self { key: key(secret), direction, counter: AtomicU64::new(0) }
    }

    pub(super) fn seal(&self, packet: &Packet) -> Vec<u8> {
        let counter = self.counter.fetch_add(1, Ordering::Relaxed);
        let mut out = Vec::with_capacity(MAX_PACKET);
        out.extend(counter.to_be_bytes());
        let mut body = Vec::with_capacity(MAX_PACKET);
        packet.encode(&mut body);
        // 只在计数用完（2^64 个包）时失败，到不了。
        self.key
            .seal_in_place_append_tag(nonce(self.direction, counter), Aad::empty(), &mut body)
            .expect("sealing a datagram");
        out.extend(body);
        out
    }
}

/// 解开对面发来的包：认证不过、格式不对、收过的（重放的）包都不要。
pub(super) struct Opener {
    key: LessSafeKey,
    direction: Direction,
    replay: Replay,
}

impl Opener {
    /// 解开 `direction` 那一头发来的包。
    pub(super) fn new(secret: &[u8; 32], direction: Direction) -> Self {
        Self { key: key(secret), direction, replay: Replay::default() }
    }

    pub(super) fn open<'a>(&mut self, datagram: &'a mut [u8]) -> Option<Packet<'a>> {
        if datagram.len() < COUNTER + TAG + 1 {
            return None;
        }
        let (counter, body) = datagram.split_at_mut(COUNTER);
        let counter = u64::from_be_bytes(counter.try_into().ok()?);
        if !self.replay.fresh(counter) {
            return None;
        }
        let plain = self.key.open_in_place(nonce(self.direction, counter), Aad::empty(), body).ok()?;
        self.replay.mark(counter);
        Packet::decode(plain)
    }
}

/// 防重放：记着收到的最大计数和它前面 `WINDOW` 个收没收过，更早的一律不要。乱序在这个窗口里的包照收。
#[derive(Default)]
struct Replay {
    /// 收到的最大计数加一，0 是还没收过。
    top: u64,
    /// 第 i 位是计数 `top - 1 - i` 收过了。
    seen: u128,
}

const WINDOW: u64 = 128;

impl Replay {
    fn fresh(&self, counter: u64) -> bool {
        if counter >= self.top {
            return true;
        }
        let back = self.top - 1 - counter;
        back < WINDOW && self.seen & (1 << back) == 0
    }

    fn mark(&mut self, counter: u64) {
        if counter >= self.top {
            let shift = counter + 1 - self.top;
            self.seen = if shift >= WINDOW { 0 } else { self.seen << shift };
            self.seen |= 1;
            self.top = counter + 1;
        } else {
            self.seen |= 1 << (self.top - 1 - counter);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: [u8; 32] = [7; 32];

    #[test]
    fn packets_round_trip() {
        let data = [9u8; 30];
        for packet in [
            Packet::Probe,
            Packet::Ping { sent_us: 123, heard: true },
            Packet::Pong { sent_us: 456, last_frame: Some(9) },
            Packet::Pong { sent_us: 456, last_frame: None },
            Packet::Video { frame: 3, index: 1, count: 4, data: &data },
            Packet::Nack { frame: 3, missing: vec![0, 2] },
            Packet::Nack { frame: 5, missing: Vec::new() },
        ] {
            let mut sealed = Sealer::new(&SECRET, Direction::FromHost).seal(&packet);
            let mut opener = Opener::new(&SECRET, Direction::FromHost);
            assert_eq!(opener.open(&mut sealed), Some(packet));
        }
    }

    /// 别的密钥、另一个方向、改过的包都解不开。
    #[test]
    fn rejects_forged_packets() {
        let sealer = Sealer::new(&SECRET, Direction::FromViewer);
        let packet = Packet::Ping { sent_us: 1, heard: false };
        assert_eq!(Opener::new(&[8; 32], Direction::FromViewer).open(&mut sealer.seal(&packet)), None);
        assert_eq!(Opener::new(&SECRET, Direction::FromHost).open(&mut sealer.seal(&packet)), None);
        let mut sealed = sealer.seal(&packet);
        let last = sealed.len() - 1;
        sealed[last] ^= 1;
        assert_eq!(Opener::new(&SECRET, Direction::FromViewer).open(&mut sealed), None);
    }

    /// 同一个包收第二次不要；乱序在窗口里的照收，太早的不要。
    #[test]
    fn rejects_replayed_packets() {
        let sealer = Sealer::new(&SECRET, Direction::FromHost);
        let packets: Vec<Vec<u8>> = (0..200).map(|_| sealer.seal(&Packet::Probe)).collect();
        let mut opener = Opener::new(&SECRET, Direction::FromHost);
        assert!(opener.open(&mut packets[5].clone()).is_some());
        assert!(opener.open(&mut packets[5].clone()).is_none());
        assert!(opener.open(&mut packets[3].clone()).is_some(), "reordered within the window");
        assert!(opener.open(&mut packets[150].clone()).is_some());
        assert!(opener.open(&mut packets[4].clone()).is_none(), "too old for the window");
        assert!(opener.open(&mut packets[149].clone()).is_some());
        assert!(opener.open(&mut packets[149].clone()).is_none());
    }

    #[test]
    fn a_full_video_packet_fits() {
        let data = vec![0u8; CHUNK];
        let sealed = Sealer::new(&SECRET, Direction::FromHost).seal(&Packet::Video {
            frame: 0,
            index: 0,
            count: 1,
            data: &data,
        });
        assert_eq!(sealed.len(), MAX_PACKET);
        let missing = vec![0u16; MAX_NACKED];
        let sealed = Sealer::new(&SECRET, Direction::FromViewer).seal(&Packet::Nack { frame: 0, missing });
        assert!(sealed.len() <= MAX_PACKET);
    }
}
