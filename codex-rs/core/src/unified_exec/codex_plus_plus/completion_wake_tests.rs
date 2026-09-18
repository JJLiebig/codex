use super::*;
use pretty_assertions::assert_eq;

#[test]
fn batches_preserve_overflow_and_replaced_claims() {
    let wake = CompletionWake::default();
    for id in 0..10 {
        wake.processes.lock().unwrap().insert(
            id,
            CompletionEntry {
                process: Weak::new(),
                result: Some(BackgroundProcessExit {
                    session_id: id,
                    exit_code: Some(0),
                    output_tail: format!("result-{id}"),
                    truncated: false,
                }),
                claim: None,
            },
        );
    }
    wake.idle_wait_interrupted.store(true, Ordering::Release);
    wake.cancel_for_abort(&codex_protocol::protocol::TurnAbortReason::Replaced);
    let (first, _) = wake.claim_input(/*interrupted*/ false).unwrap();
    let mut current_claim = Some(first);
    assert!(
        wake.input_after_idle_wait(/*interrupted*/ false, &mut current_claim)
            .is_empty()
    );
    assert_eq!(current_claim, Some(first));
    assert_eq!(
        wake.processes
            .lock()
            .unwrap()
            .values()
            .filter(|entry| entry.claim == Some(first))
            .count(),
        8
    );
    assert!(wake.has_ready());
    wake.commit_claim(first);
    current_claim = None;
    assert!(
        !wake
            .input_after_idle_wait(/*interrupted*/ false, &mut current_claim)
            .is_empty()
    );
    let second = current_claim.unwrap();
    assert!(!wake.has_ready());
    wake.cancel_for_abort(&codex_protocol::protocol::TurnAbortReason::Replaced);
    let (replacement, _) = wake.claim_input(/*interrupted*/ false).unwrap();
    assert_ne!(second, replacement);
    assert_eq!(
        wake.processes
            .lock()
            .unwrap()
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![8, 9]
    );
    wake.commit_claim(replacement);
    assert!(wake.claim_input(/*interrupted*/ false).is_none());
}
