use super::*;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::keymap::RuntimeKeymap;
use crate::test_backend::VT100Backend;
use pretty_assertions::assert_eq;
use ratatui::Terminal;
use tokio::sync::mpsc::unbounded_channel;

#[test]
fn cache_deadline_picker_enables_and_disables_exact_model() {
    for (seconds, key, expected) in [(0, KeyCode::Down, 3300), (3300, KeyCode::Up, 0)] {
        let (tx, mut rx) = unbounded_channel();
        let mut view = ListSelectionView::new(
            interval_params("gpt-6-astra".into(), seconds),
            AppEventSender::new(tx),
            RuntimeKeymap::defaults().list,
        );
        if seconds == 0 {
            let mut terminal =
                Terminal::new(VT100Backend::new(/*width*/ 84, /*height*/ 14)).unwrap();
            terminal
                .draw(|frame| view.render(frame.area(), frame.buffer_mut()))
                .unwrap();
            insta::assert_snapshot!(
                "cache_deadline_interval",
                terminal
                    .backend()
                    .to_string()
                    .lines()
                    .map(str::trim_end)
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }
        view.handle_key_event(KeyEvent::from(key));
        view.handle_key_event(KeyEvent::from(KeyCode::Enter));
        let AppEvent::PersistCacheDeadline { model, seconds } = rx.try_recv().unwrap() else {
            panic!("expected interval save")
        };
        assert_eq!((model.as_str(), seconds), ("gpt-6-astra", expected));
    }
}

#[test]
fn cache_deadline_custom_duration_preserves_seconds_and_rejects_bad_input() {
    for (text, expected) in [
        ("55m", Some(3300)),
        (" 3s ", Some(3)),
        ("1h", Some(3600)),
        ("0s", None),
        ("-5m", None),
        ("5", None),
        ("18446744073709551615h", None),
        ("∞m", None),
    ] {
        assert_eq!(parse_duration(text), expected);
    }
}

#[tokio::test]
async fn cache_deadline_model_picker_and_custom_prompt_keep_model_identity() {
    use crate::chatwidget::tests::helpers::make_chatwidget_manual;
    use crate::chatwidget::tests::helpers::render_bottom_popup;
    let (mut chat, mut events, _ops) = make_chatwidget_manual(Some("gpt-6-astra")).await;
    let template = chat.model_catalog.models[0].clone();
    Arc::make_mut(&mut chat.model_catalog).models =
        [("gpt-6-astra", "Astra"), ("gpt-6.1-sol", "Sol")]
            .into_iter()
            .map(|(model, name)| {
                let mut preset = template.clone();
                preset.model = model.to_string();
                preset.display_name = name.to_string();
                preset.show_in_picker = true;
                preset
            })
            .collect();
    chat.open_cache_deadline_models();
    insta::assert_snapshot!(
        "cache_deadline_models",
        render_bottom_popup(&chat, /*width*/ 84)
    );
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let AppEvent::OpenCacheDeadlineInterval(model) = events.try_recv().unwrap() else {
        panic!("expected model selection")
    };
    assert_eq!(model, "gpt-6.1-sol");
    chat.open_cache_deadline_interval(model);
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let AppEvent::OpenCacheDeadlineCustom(model) = events.try_recv().unwrap() else {
        panic!("expected custom interval")
    };
    chat.open_cache_deadline_custom(model);
    insta::assert_snapshot!(
        "cache_deadline_custom",
        render_bottom_popup(&chat, /*width*/ 84)
    );
    chat.handle_key_event(KeyEvent::new(
        KeyCode::Char('u'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    chat.handle_paste("1h".to_string());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let AppEvent::InsertHistoryCell(error) = events.try_recv().unwrap() else {
        panic!("expected known cache lifetime error")
    };
    insta::assert_snapshot!(
        "cache_deadline_custom_error",
        error
            .display_lines(84)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
    chat.open_cache_deadline_custom("gpt-6.1-sol".into());
    chat.handle_key_event(KeyEvent::new(
        KeyCode::Char('u'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    chat.handle_paste("3s".to_string());
    chat.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let AppEvent::PersistCacheDeadline { model, seconds } = events.try_recv().unwrap() else {
        panic!("expected interval save")
    };
    assert_eq!((model.as_str(), seconds), ("gpt-6.1-sol", 3));
}
