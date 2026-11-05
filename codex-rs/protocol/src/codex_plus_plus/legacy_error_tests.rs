use crate::protocol::ErrorEvent;
#[test]
fn historical_attributed_errors_remain_terminal_after_decode_and_rewrite() {
    for attribution in [
        serde_json::json!({"type":"claude"}),
        serde_json::json!({"type":"unknown"}),
    ] {
        let mut value = serde_json::json!({"message":"failed", "codex_error_info":"bad_request"});
        let plain: ErrorEvent = serde_json::from_value(value.clone()).unwrap();
        assert!(!plain.affects_turn_status());
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
