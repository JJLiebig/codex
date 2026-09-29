use super::*;

#[tokio::test]
async fn owned_model_picker_clears_successful_empty_catalogue() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.5")).await;
    chat.config.model_provider =
        codex_model_provider_info::ModelProviderInfo::create_cli_proxy_provider();
    chat.thread_id = Some(ThreadId::new());
    chat.open_model_popup();
    let request = chat.model_popup_request_id.unwrap();
    while rx.try_recv().is_ok() {}
    assert!(chat.on_models_loaded(request, Ok(Vec::new())));
    assert!(chat.model_catalog.try_list_models().unwrap().is_empty());
    assert_eq!(chat.bottom_pane.active_view_id(), None);
    let cell = assert_matches!(rx.try_recv(), Ok(AppEvent::InsertHistoryCell(cell)) => cell);
    insta::assert_snapshot!(
        lines_to_single_string(&cell.display_lines(/*width*/ 80)),
        @"• No additional models are available right now."
    );
}
