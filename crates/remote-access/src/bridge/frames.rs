//! 桥接时两个方向上的帧边界。字节照旧一块一块地搬，这里只跟着帧头数位置：
//!
//! - 手机 → 宿主（`FromPhone`）：控制帧整帧攒齐、解出 `ClientMsg`，手机连接不能做的（见
//!   `refusal`）不转给宿主，改给手机回一条 `HostMsg::Error`；别的帧（输入等）不解析，边到边转。
//! - 宿主 → 手机（`ToPhone`）：只数帧头里的长度，知道现在是不是正好在两帧之间，回给手机的
//!   `Error` 只在那里插进去，不会把宿主的帧拆开。

use runode_protocol::{
    ClientMsg, FrameKind, HostMsg, MAX_PAYLOAD,
    frame::{FrameError, HEADER_LEN},
    write_frame,
};

/// 攒控制帧时一开始最多先要这么多内存，再多的随到达的字节长，不照对面声明的长度一上来就分配。
const HOLD_RESERVE: usize = 64 << 10;

/// 手机发来的明文按帧过一遍，见模块文档。
#[derive(Default)]
pub(crate) struct FromPhone {
    header: [u8; HEADER_LEN],
    /// 帧头已经到了几个字节；`held` 不为空、或者 `left` 大于 0 时在帧体里。
    have: usize,
    /// 这一帧的帧体还差几个字节。
    left: u32,
    /// 正在攒的控制帧（含帧头）；不是控制帧时为 `None`，帧体边到边转。
    held: Option<Vec<u8>>,
}

impl FromPhone {
    /// 过一段明文：要转给宿主的追加到 `to_host`，挡下的控制消息给手机的回话（编好的帧）追加到
    /// `refusals`。帧头声明的载荷超过 `MAX_PAYLOAD` 时报错，连接上的数据已经对不齐了。
    pub(crate) fn feed(
        &mut self,
        mut bytes: &[u8],
        to_host: &mut Vec<u8>,
        refusals: &mut Vec<u8>,
    ) -> Result<(), FrameError> {
        while !bytes.is_empty() {
            if self.left == 0 && self.held.is_none() {
                let take = (HEADER_LEN - self.have).min(bytes.len());
                self.header[self.have..self.have + take].copy_from_slice(&bytes[..take]);
                self.have += take;
                bytes = &bytes[take..];
                if self.have < HEADER_LEN {
                    continue;
                }
                self.have = 0;
                let len = u32::from_le_bytes([self.header[0], self.header[1], self.header[2], self.header[3]]);
                if len > MAX_PAYLOAD {
                    return Err(FrameError::TooLong(u64::from(len)));
                }
                self.left = len;
                if self.header[4] == FrameKind::Control as u8 {
                    let mut held = Vec::with_capacity(HEADER_LEN + (len as usize).min(HOLD_RESERVE));
                    held.extend_from_slice(&self.header);
                    self.held = Some(held);
                    if len == 0 {
                        self.decide(to_host, refusals);
                    }
                } else {
                    to_host.extend_from_slice(&self.header);
                }
                continue;
            }
            let take = (self.left as usize).min(bytes.len());
            match &mut self.held {
                Some(held) => held.extend_from_slice(&bytes[..take]),
                None => to_host.extend_from_slice(&bytes[..take]),
            }
            self.left -= take as u32;
            bytes = &bytes[take..];
            if self.left == 0 && self.held.is_some() {
                self.decide(to_host, refusals);
            }
        }
        Ok(())
    }

    /// 攒齐了一个控制帧：手机连接不能做的挡下、回一条 `Error`，别的原样转给宿主（解不出来的也转，
    /// 由宿主照常回话）。
    fn decide(&mut self, to_host: &mut Vec<u8>, refusals: &mut Vec<u8>) {
        let Some(frame) = self.held.take() else { return };
        let refused = serde_json::from_slice::<ClientMsg>(&frame[HEADER_LEN..]).ok().as_ref().and_then(refusal);
        let Some(what) = refused else {
            to_host.extend_from_slice(&frame);
            return;
        };
        tracing::info!("refused a remote device's request to {what}");
        let error = HostMsg::Error { req: None, id: None, message: format!("a remote device cannot {what}") };
        match serde_json::to_vec(&error) {
            Ok(payload) => {
                // 写进 `Vec` 不会出错，载荷也远小于上限。
                let _ = write_frame(refusals, FrameKind::Control, 0, &payload);
            }
            Err(err) => tracing::warn!("cannot encode a refusal: {err}"),
        }
    }
}

/// 手机连接不能做的事，返回给它的说明里「不能做什么」那半句；能做的为 `None`。这些都是管宿主本身、
/// 或者只有本机的 app 才该做的：让宿主退出（`Shutdown`）、升级交接、替界面回话（`UiReply`）、改
/// 宿主的选项和主题。几种都不带 `req` 和会话，回的 `Error` 也就不带。
pub(crate) fn refusal(message: &ClientMsg) -> Option<&'static str> {
    Some(match message {
        ClientMsg::Shutdown { .. } => "shut the host down",
        ClientMsg::Handoff { .. }
        | ClientMsg::HandoffReady
        | ClientMsg::HandoffAbort { .. }
        | ClientMsg::HandoffDone => "hand the host over",
        ClientMsg::UiReply { .. } => "answer for the runode window",
        ClientMsg::SetOptions { .. } => "change the host's options",
        ClientMsg::SetTheme { .. } => "change the terminal theme",
        _ => return None,
    })
}

/// 宿主发给手机的字节流现在走到哪一帧的哪里，见模块文档。
#[derive(Default)]
pub(crate) struct ToPhone {
    /// 帧头里载荷长度的那 4 个字节，到了几个记几个。
    len: [u8; 4],
    /// 帧头已经过去几个字节；为 0 且 `left` 为 0 时正好在两帧之间。
    have: usize,
    /// 这一帧的帧体还差几个字节。
    left: usize,
}

impl ToPhone {
    /// 正好在两帧之间。
    pub(crate) fn at_boundary(&self) -> bool {
        self.have == 0 && self.left == 0
    }

    /// 再过多少字节到这一帧的帧头结尾或者帧体结尾；在两帧之间时为 0。帧头没到齐时还不知道帧体多长，
    /// 先只到帧头结尾。
    pub(crate) fn to_next_stop(&self) -> usize {
        if self.left > 0 {
            self.left
        } else if self.have > 0 {
            HEADER_LEN - self.have
        } else {
            0
        }
    }

    /// 这些字节交给手机了。
    pub(crate) fn advance(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            if self.left > 0 {
                let take = self.left.min(bytes.len());
                self.left -= take;
                bytes = &bytes[take..];
                continue;
            }
            let take = (HEADER_LEN - self.have).min(bytes.len());
            for (i, &byte) in bytes[..take].iter().enumerate() {
                if let Some(slot) = self.len.get_mut(self.have + i) {
                    *slot = byte;
                }
            }
            self.have += take;
            bytes = &bytes[take..];
            if self.have == HEADER_LEN {
                self.have = 0;
                self.left = u32::from_le_bytes(self.len) as usize;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use runode_protocol::{Frame, read_frame};

    use super::*;

    fn frame(kind: FrameKind, channel: u32, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_frame(&mut out, kind, channel, payload).unwrap();
        out
    }

    fn control(json: &str) -> Vec<u8> {
        frame(FrameKind::Control, 0, json.as_bytes())
    }

    fn frames(mut bytes: &[u8]) -> Vec<Frame> {
        let mut out = Vec::new();
        while let Some(frame) = read_frame(&mut bytes).unwrap() {
            out.push(frame);
        }
        out
    }

    /// 一段流按 `split` 字节一块喂进去，结果和一次喂完一样。
    fn feed_in_pieces(stream: &[u8], split: usize) -> (Vec<u8>, Vec<u8>) {
        let mut phone = FromPhone::default();
        let (mut to_host, mut refusals) = (Vec::new(), Vec::new());
        for piece in stream.chunks(split) {
            phone.feed(piece, &mut to_host, &mut refusals).unwrap();
        }
        (to_host, refusals)
    }

    #[test]
    fn host_level_requests_are_refused_and_the_rest_passes() {
        let allowed = [
            control(r#"{"type":"list_sessions"}"#),
            frame(FrameKind::Input, 3, b"ls\r"),
            control(r#"{"type":"kill","id":"0123456789abcdef0123456789abcdef"}"#),
            frame(FrameKind::Input, 3, b""),
            control(r#"{"type":"something_newer"}"#),
            control("not json"),
            control(r#"{"type":"list_dirs","req":5,"path":null}"#),
            control(r#"{"type":"open_workspace","req":8,"dir":"/Users/me/dev","focus":false}"#),
        ];
        let refused = [
            control(r#"{"type":"shutdown","kill_sessions":true}"#),
            control(r#"{"type":"handoff","min_format":1,"max_format":1}"#),
            control(r#"{"type":"handoff_ready"}"#),
            control(r#"{"type":"handoff_abort"}"#),
            control(r#"{"type":"handoff_done"}"#),
            control(r#"{"type":"set_options","record_history":false}"#),
            control(r#"{"type":"ui_reply","ui":1,"reply":{"type":"done","req":1}}"#),
        ];
        let mut stream = Vec::new();
        for (allowed, refused) in allowed.iter().zip(refused.iter().chain(std::iter::repeat(&Vec::new()))) {
            stream.extend_from_slice(allowed);
            stream.extend_from_slice(refused);
        }
        for split in [1, 2, 5, 9, 10, 64, stream.len()] {
            let (to_host, refusals) = feed_in_pieces(&stream, split);
            assert_eq!(to_host, allowed.concat(), "split {split}");
            let errors = frames(&refusals);
            assert_eq!(errors.len(), refused.len(), "split {split}");
            for error in errors {
                match error.message::<HostMsg>().unwrap() {
                    HostMsg::Error { req: None, id: None, message } => {
                        assert!(message.starts_with("a remote device cannot"), "{message}");
                    }
                    other => panic!("{other:?}"),
                }
            }
        }
    }

    #[test]
    fn a_frame_over_the_limit_is_an_error() {
        let mut header = [0u8; HEADER_LEN];
        header[..4].copy_from_slice(&(MAX_PAYLOAD + 1).to_le_bytes());
        header[4] = FrameKind::Input as u8;
        let mut phone = FromPhone::default();
        assert!(phone.feed(&header, &mut Vec::new(), &mut Vec::new()).is_err());
    }

    #[test]
    fn the_host_stream_is_tracked_frame_by_frame() {
        let stream = [
            frame(FrameKind::Output, 1, &[7; 300]),
            frame(FrameKind::Control, 0, b""),
            frame(FrameKind::Snapshot, 2, &[1; 20]),
        ]
        .concat();
        let boundaries = [0, HEADER_LEN + 300, 2 * HEADER_LEN + 300, stream.len()];
        for split in [1, 3, 9, 100, stream.len()] {
            let mut tracker = ToPhone::default();
            let mut at = 0;
            for piece in stream.chunks(split) {
                // 一块里每个字节过去以后，在不在两帧之间都要和真正的边界对上。
                for byte in piece {
                    tracker.advance(std::slice::from_ref(byte));
                    at += 1;
                    assert_eq!(tracker.at_boundary(), boundaries.contains(&at), "split {split}, at {at}");
                }
            }
        }
        let mut tracker = ToPhone::default();
        tracker.advance(&stream[..4]);
        assert_eq!(tracker.to_next_stop(), HEADER_LEN - 4);
        tracker.advance(&stream[4..HEADER_LEN + 10]);
        assert_eq!(tracker.to_next_stop(), 290);
    }
}
