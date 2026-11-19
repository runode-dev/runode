//! LSP 的基础协议：每条消息前面是 `Content-Length` 头和一个空行，后面是这么多字节的 JSON。

use std::io::{self, BufRead, Write};

pub(crate) fn write_message(out: &mut impl Write, body: &[u8]) -> io::Result<()> {
    write!(out, "Content-Length: {}\r\n\r\n", body.len())?;
    out.write_all(body)?;
    out.flush()
}

/// 读下一条消息的正文；对方在两条消息之间关了流时是 `None`。
pub(crate) fn read_message(input: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let invalid = |why: &str| io::Error::new(io::ErrorKind::InvalidData, why.to_owned());
    let mut length = None;
    let mut started = false;
    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            return if started { Err(io::ErrorKind::UnexpectedEof.into()) } else { Ok(None) };
        }
        started = true;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = Some(value.trim().parse::<usize>().map_err(|_| invalid("bad Content-Length"))?);
        }
    }
    let mut body = vec![0; length.ok_or_else(|| invalid("no Content-Length"))?];
    input.read_exact(&mut body)?;
    Ok(Some(body))
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Read};

    use super::*;

    /// 一次只给一个字节的流：头和正文都被切碎。
    struct Trickle<'a>(&'a [u8]);

    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some((first, rest)) = self.0.split_first() else {
                return Ok(0);
            };
            buf[0] = *first;
            self.0 = rest;
            Ok(1)
        }
    }

    #[test]
    fn messages_survive_being_glued_together_and_cut_apart() {
        let mut wire = Vec::new();
        write_message(&mut wire, b"{\"a\":1}").unwrap();
        write_message(&mut wire, "{\"b\":\"中\"}".as_bytes()).unwrap();
        let whole: Box<dyn Read> = Box::new(&wire[..]);
        let trickle: Box<dyn Read> = Box::new(Trickle(&wire));
        for input in [whole, trickle] {
            let mut input = BufReader::new(input);
            assert_eq!(read_message(&mut input).unwrap().unwrap(), b"{\"a\":1}");
            assert_eq!(read_message(&mut input).unwrap().unwrap(), "{\"b\":\"中\"}".as_bytes());
            assert!(read_message(&mut input).unwrap().is_none());
        }
    }

    #[test]
    fn other_headers_are_skipped_and_the_length_is_required() {
        let mut input = &b"Content-Type: x\r\ncontent-length: 2\r\n\r\n{}"[..];
        assert_eq!(read_message(&mut input).unwrap().unwrap(), b"{}");
        let mut input = &b"Content-Type: x\r\n\r\n{}"[..];
        assert_eq!(read_message(&mut input).unwrap_err().kind(), io::ErrorKind::InvalidData);
        let mut input = &b"Content-Length: 5\r\n\r\n{}"[..];
        assert_eq!(read_message(&mut input).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }
}
