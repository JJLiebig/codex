use super::*;
use pretty_assertions::assert_eq;

#[test]
fn program_status_negotiation_preserves_paste_and_obeys_da_order() {
    for reply in [
        b"\x1b]7501;?\x07".as_slice(),
        b"\x1b]7501;?:future=yes\x1b\\".as_slice(),
    ] {
        let mut input = b"typed\x1b[200~\x1b]7501;?\x07\x1b[?1c\x1b[201~".to_vec();
        assert_eq!(detected_support(&input), None);
        input.extend_from_slice(reply);
        assert_eq!(detected_support(&input), Some(true));
        assert!(!sentinel_received(&input));
        input.extend_from_slice(b"\x1b[?64;1c");
        assert!(sentinel_received(&input));
        assert_eq!(detected_support(&input), Some(true));
        let ranges = response_ranges(&input);
        assert_eq!(ranges.len(), 2);
        assert_eq!(&input[ranges[0].clone()], reply);
    }
    assert_eq!(detected_support(b"\x1b[?64;1c\x1b]7501;?\x07"), Some(false));
    assert_eq!(detected_support(b"\x1b]7501;?"), None);
}

#[test]
fn program_status_messages_are_safe_bounded_utf8() {
    let status = Status {
        state: State::Blocked,
        kind: Some(Kind::Permission),
        message: Some(format!("\x1b\x07\u{009c}\n{}", "🦀".repeat(/*n*/ 700))),
    };
    let report = status.encode();
    assert!(report.len() < 4096);
    let encoded = report
        .split("msg=")
        .nth(1)
        .unwrap()
        .strip_suffix("\x1b\\")
        .unwrap();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let message = String::from_utf8(decoded).unwrap();
    assert!(message.len() <= 2048);
    assert!(!message.chars().any(char::is_control));
    assert!(message.chars().all(|ch| ch == '🦀'));
}

#[test]
fn program_status_writes_changes_and_republishes_after_handoff() {
    let mut reporter = Reporter::default();
    let mut output = Vec::new();
    let status = Status {
        state: State::Working,
        kind: None,
        message: Some("Session".into()),
    };
    reporter
        .write(&mut output, /*generation*/ 0, status.clone())
        .unwrap();
    reporter
        .write(&mut output, /*generation*/ 0, status.clone())
        .unwrap();
    reporter
        .write(&mut output, /*generation*/ 1, status)
        .unwrap();
    assert_eq!(
        String::from_utf8(output).unwrap(),
        "\x1b]7501;state=working:app=codex:msg=U2Vzc2lvbg==\x1b\\".repeat(/*n*/ 2)
    );
}
