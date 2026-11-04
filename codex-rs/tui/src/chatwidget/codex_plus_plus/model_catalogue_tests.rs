use super::*;

#[tokio::test]
async fn owned_model_picker_clears_successful_empty_catalogue() {
    for reasoning_submenu in [false, true] {
        let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.5")).await;
        chat.config.model_provider =
            codex_model_provider_info::ModelProviderInfo::create_cli_proxy_provider();
        chat.thread_id = Some(ThreadId::new());
        let mut preset = get_available_model(&chat, "gpt-5.5");
        chat.open_model_popup();
        let request = chat.model_popup_request_id.unwrap();
        if reasoning_submenu {
            chat.open_reasoning_popup(preset.clone());
            preset.default_reasoning_effort = ReasoningEffortConfig::Max;
            chat.open_advanced_reasoning_popup(preset.clone());
            chat.open_plan_reasoning_scope_prompt(preset.model, Some(ReasoningEffortConfig::Max));
            chat.bottom_pane.show_selection_view(SelectionViewParams {
                view_id: Some("unrelated-dialog"),
                items: vec![SelectionItem {
                    name: "Keep open".into(),
                    ..Default::default()
                }],
                ..SelectionViewParams::picker()
            });
        }
        while rx.try_recv().is_ok() {}
        assert!(chat.on_models_loaded(request, Ok(Vec::new())));
        assert!(chat.model_catalog.try_list_models().unwrap().is_empty());
        if reasoning_submenu {
            assert_eq!(chat.bottom_pane.active_view_id(), Some("unrelated-dialog"));
            chat.bottom_pane.dismiss_view_by_id("unrelated-dialog");
        }
        assert!(chat.no_modal_or_popup_active());
        let cell = assert_matches!(rx.try_recv(), Ok(AppEvent::InsertHistoryCell(cell)) => cell);
        insta::allow_duplicates! {
            insta::assert_snapshot!(
                lines_to_single_string(&cell.display_lines(/*width*/ 80)),
                @"• No additional models are available right now."
            );
        }
    }
}
