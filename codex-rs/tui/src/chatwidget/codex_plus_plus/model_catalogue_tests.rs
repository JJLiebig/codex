use super::*;

#[tokio::test]
async fn owned_claude_picker_refresh_removes_obsolete_choices() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("claude-sonnet-5-5")).await;
    chat.config.model_provider =
        codex_model_provider_info::ModelProviderInfo::create_cli_proxy_provider();
    chat.thread_id = Some(ThreadId::new());
    let template = get_available_model(&chat, "gpt-5.5");
    let models: Vec<_> = [
        ("claude-fable-5", "Claude Fable 5"),
        ("claude-fable-5-1", "Claude Fable 5.1"),
        ("claude-opus-5", "Claude Opus 5"),
        ("claude-opus-5-5", "Claude Opus 5.5"),
        ("claude-sonnet-5", "Claude Sonnet 5"),
        ("claude-sonnet-5-5", "Claude Sonnet 5.5"),
        ("claude-haiku-4-5-20251001", "Claude Haiku 4.5"),
    ]
    .into_iter()
    .map(|(slug, label)| {
        let mut model = template.clone();
        model.id = slug.into();
        model.model = slug.into();
        model.display_name = label.into();
        model.description.clear();
        model.is_default = false;
        model
    })
    .collect();
    let mut old = template;
    old.model = "claude-sonnet-4-6".into();
    old.display_name = "Claude Sonnet 4.6".into();
    Arc::make_mut(&mut chat.model_catalog).models = vec![old];
    chat.open_model_popup();
    let request = chat.model_popup_request_id.unwrap();
    assert!(chat.on_models_loaded(request, Ok(models)));
    insta::assert_snapshot!(render_bottom_popup(&chat, /*width*/ 80), @"
      Select Model and Effort


      1. Claude Fable 5
      2. Claude Fable 5.1
      3. Claude Opus 5
      4. Claude Opus 5.5
      5. Claude Sonnet 5
    › 6. Claude Sonnet 5.5 (current)
      7. Claude Haiku 4.5

      enter select · esc back
    ");
}

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
