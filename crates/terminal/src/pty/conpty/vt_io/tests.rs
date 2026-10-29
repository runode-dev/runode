use super::{OutputFilter, encode_reply};

fn filtered(chunks: &[&[u8]]) -> Vec<u8> {
    let mut filter = OutputFilter::default();
    let mut output = Vec::new();
    for chunk in chunks {
        let mut chunk = chunk.to_vec();
        filter.filter(&mut chunk);
        output.extend_from_slice(&chunk);
    }
    output
}

#[test]
fn unsupported_keyboard_negotiation_is_cancelled_at_every_read_boundary() {
    let input = b"before\x1b[>7u\x1b[?u\x1b[=1;3u\x1b[<u\x1b[<2u\x1b[>4;2m\x1b[>4m\x1b[?4mafter";
    for split in 0..=input.len() {
        assert_legacy_keyboard(&filtered(&[&input[..split], &input[split..]]));
    }
    let chunks: Vec<_> = input.chunks(1).collect();
    assert_legacy_keyboard(&filtered(&chunks));
}

fn assert_legacy_keyboard(output: &[u8]) {
    use libghostty_vt::key;

    let mut terminal = libghostty_vt::Terminal::new(20, 4).unwrap();
    terminal.vt_write(output);
    assert!(terminal.kitty_keyboard_flags().unwrap().is_empty(), "{output:?}");
    let mut encoder = key::Encoder::new().unwrap();
    encoder.set_options_from_terminal(&terminal);
    let mut event = key::Event::new().unwrap();
    event.set_action(key::Action::Press).set_key(key::Key::C).set_mods(key::Mods::CTRL).set_unshifted_codepoint('c');
    let mut encoded = [0; 64];
    let len = encoder.encode(&event, &mut encoded).unwrap();
    assert_eq!(&encoded[..len], b"\x03", "{output:?}");
}

#[test]
fn ordinary_text_and_terminal_controls_are_preserved() {
    let input = "中文🙂\r\n\x1b[31mred\x1b[m\x1b[?1h\x1b[?2004h\x1b[?1049h\x1b[2J\x1b[H\x1b[u\x1b[c\x1b[6n\x1b[>0q";
    for split in 0..=input.len() {
        assert_eq!(filtered(&[&input.as_bytes()[..split], &input.as_bytes()[split..]]), input.as_bytes());
    }
}

#[test]
fn an_escape_aborts_control_strings_before_starting_a_keyboard_request() {
    for (start, end) in [
        (b"\x1b]".as_slice(), b"\x07".as_slice()),
        (b"\x1b]", b"\x1b\\"),
        (b"\x1bP", b"\x1b\\"),
        (b"\x1b_", b"\x1b\\"),
        (b"\x1b^", b"\x1b\\"),
        (b"\x1bX", b"\x1b\\"),
    ] {
        let input = [start, b"payload\x1b[>7u\x1b[>4;2m", end].concat();
        let chunks: Vec<_> = input.chunks(1).collect();
        assert_legacy_keyboard(&filtered(&chunks));
    }
}

#[test]
fn cancelled_keyboard_sequences_do_not_leave_the_vt_inside_a_string() {
    for start in [b"\x1b]0;title".as_slice(), b"\x1bPqpayload", b"\x1b_payload"] {
        let input = [start, b"\x1b[>7uOK"].concat();
        let mut terminal = libghostty_vt::Terminal::new(20, 4).unwrap();
        terminal.vt_write(&filtered(&[&input]));
        assert_eq!(crate::testing::screen_row(&terminal, 0), "OK");
    }
}

#[test]
fn controls_and_long_zero_padded_parameters_cannot_enable_keyboard_protocols() {
    for control in [0, 7, 9, 10, 13, 0x19, 0x7f] {
        for input in [
            [b"\x1b[>7".as_slice(), &[control], b"u"].concat(),
            [b"\x1b".as_slice(), &[control], b"[>7u"].concat(),
            [b"\x1b[>4;".as_slice(), &[control], b"2m"].concat(),
        ] {
            let output = filtered(&[&input]);
            assert!(output.contains(&control));
            assert_legacy_keyboard(&output);
        }
    }
    for len in [127, 128, 129, 255, 256, 4096] {
        for tail in [b"7u".as_slice(), b"4;2m"] {
            let input = [b"\x1b[>".as_slice(), &b"0".repeat(len), tail].concat();
            let chunks: Vec<_> = input.chunks(1).collect();
            assert_legacy_keyboard(&filtered(&chunks));
        }
    }
    // 同样长的普通 CSI 不改写，只有不支持的键盘请求需要取消。
    let input = [b"\x1b[".as_slice(), &b"0".repeat(4096), b"31mred"].concat();
    assert_eq!(filtered(&[&input]), input);
}

#[test]
fn cancelled_and_unfinished_sequences_do_not_drop_output() {
    for input in [
        b"end\x1b".as_slice(),
        b"end\x1b[>",
        b"\x1b[>7\x18text",
        b"\x1b[>7\x1atext",
        b"\x1b\x1b[31mtext",
        b"\x1b[\x1b[31mtext",
    ] {
        let chunks: Vec<_> = input.chunks(1).collect();
        assert_eq!(filtered(&chunks), input);
    }
}

#[test]
fn filtered_output_keeps_host_and_frontend_key_encoders_in_legacy_mode() {
    use crate::session::Request;
    use crate::testing::{capturing_session, idle_host};
    use runode_types::input::{Key, KeyChord, KeyInput, Mods};

    let requests = filtered(&[b"\x1b[>7u\x1b[?u\x1b[>4;2m"]);
    let mut host = idle_host();
    host.feed(&requests);
    let (mut session, sent) = capturing_session();
    session.feed(&requests);
    for (name, key, mods, expected) in [
        ("up", Key::ArrowUp, Mods::default(), b"\x1b[A".as_slice()),
        ("down", Key::ArrowDown, Mods::default(), b"\x1b[B"),
        ("esc", Key::Escape, Mods::default(), b"\x1b"),
        ("ctrl-c", Key::C, Mods { ctrl: true, ..Mods::default() }, b"\x03"),
    ] {
        let chord: KeyChord = name.parse().unwrap();
        assert_eq!(host.encode_keys(&[chord]).unwrap(), expected, "host: {name}");
        let input = KeyInput {
            key,
            mods,
            consumed_mods: Mods::default(),
            unshifted: key.unshifted_char().unwrap_or('\0'),
            text: None,
            layout_option_as_alt: false,
        };
        assert!(session.key(&input));
        assert!(
            matches!(sent.borrow_mut().pop(), Some(Request::Input(bytes)) if bytes == expected),
            "frontend: {name}"
        );
    }
}

#[test]
fn osc_replies_use_unicode_input_records() {
    for reply in [
        b"\x1b]11;rgb:ffff/ffff/ffff\x07".as_slice(),
        b"\x1b]4;1;rgb:ffff/0000/0000\x1b\\",
        b"\x1b[?62;22c\x1b]10;rgb:ffff/ffff/ffff\x07",
    ] {
        let encoded = encode_reply(reply);
        assert_ne!(encoded.as_ref(), reply);
        let decoded: Vec<u8> = std::str::from_utf8(&encoded)
            .unwrap()
            .split("\x1b[0;0;")
            .skip(1)
            .filter_map(|record| record.strip_suffix(";1;0;1_"))
            .map(|unit| unit.parse::<u8>().unwrap())
            .collect();
        assert_eq!(decoded, reply);
    }
    for reply in [
        b"\x1b[12;34R".as_slice(),
        b"\x1b[?62;22c",
        b"\x1b[?2004;1$y",
        b"\x1b[?997;1n",
        b"\x1bP>|runode 0.1\x1b\\",
        b"\x1bP1$r0m\x1b\\",
    ] {
        assert_eq!(encode_reply(reply).as_ref(), reply);
    }
}
