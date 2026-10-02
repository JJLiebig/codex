use super::*;
use crate::test_backend::VT100Backend;
use crossterm::event::KeyEvent;
use pretty_assertions::assert_eq;
use ratatui::Terminal;

#[tokio::test]
async fn removal_requires_explicit_confirmation() -> color_eyre::Result<()> {
    for (keys, confirmed) in [
        (vec![KeyCode::Enter], false),
        (vec![KeyCode::Down, KeyCode::Enter], true),
        (vec![KeyCode::Down, KeyCode::Esc], false),
        (vec![], false),
    ] {
        let mut tui = tui::test_support::make_test_tui()?;
        let events = tokio_stream::iter(
            keys.into_iter()
                .map(|code| TuiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))),
        );
        assert_eq!(
            confirm_removal(&mut tui, "first@example.com", events).await?,
            confirmed
        );
    }
    for code in [KeyCode::Char('c'), KeyCode::Char('d')] {
        let mut tui = tui::test_support::make_test_tui()?;
        let events =
            tokio_stream::iter([TuiEvent::Key(KeyEvent::new(code, KeyModifiers::CONTROL))]);
        assert!(!confirm_removal(&mut tui, "first@example.com", events).await?);
    }
    Ok(())
}

#[test]
fn removal_confirmation_snapshot() {
    let mut terminal = Terminal::new(VT100Backend::new(/*width*/ 100, /*height*/ 10)).unwrap();
    let view = confirmation_view("first@example.com");
    terminal
        .draw(|frame| view.render(frame.area(), frame.buffer_mut()))
        .unwrap();
    insta::assert_snapshot!("account_removal_confirmation", terminal.backend());
}
