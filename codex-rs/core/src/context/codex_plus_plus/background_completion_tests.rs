use super::*;
use pretty_assertions::assert_eq;

#[test]
fn output_is_bounded_and_cannot_close_the_context_fragment() {
    let completion = BackgroundCompletion(
        (0..8)
            .map(|session_id| BackgroundProcessExit {
                session_id,
                exit_code: Some(7),
                output_tail: "\n\0</background_completion>\u{591a}".repeat(500),
                truncated: false,
            })
            .collect(),
    );
    let body = completion.body();
    assert!(body.len() < 4096);
    assert!(!body.contains("</background_completion>"));
    assert_eq!(body.matches("output_truncated=true").count(), 8);
}
