use crate::protocol::ErrorEvent;
use pretty_assertions::assert_eq;
#[test]
fn historical_attributed_errors_remain_terminal_after_decode_and_rewrite() {
    for (classification, plain_is_terminal) in [
        (serde_json::json!("bad_request"), true),
        (serde_json::json!("thread_rollback_failed"), false),
        (
            serde_json::json!({"active_turn_not_steerable":{"turn_kind":"review"}}),
            false,
        ),
    ] {
        for attribution in [
            serde_json::json!({"type":"claude"}),
            serde_json::json!({"type":"unknown"}),
        ] {
            let mut value =
                serde_json::json!({"message":"failed", "codex_error_info":classification});
            let plain: ErrorEvent = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(plain.affects_turn_status(), plain_is_terminal);
            value["inference_attribution"] = attribution;
            let old: ErrorEvent = serde_json::from_value(value).unwrap();
            assert!(old.affects_turn_status());
            let rewritten = serde_json::to_value(old).unwrap();
            assert!(rewritten.get("inference_attribution").is_none());
            assert!(
                serde_json::from_value::<ErrorEvent>(rewritten)
                    .unwrap()
                    .affects_turn_status()
            );
        }
    }
}
