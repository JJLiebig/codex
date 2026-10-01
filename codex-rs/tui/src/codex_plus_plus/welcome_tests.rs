use ratatui::Terminal;
use ratatui::text::Text;

use super::*;
use crate::test_backend::VT100Backend;
use pretty_assertions::assert_eq;

#[test]
fn replaces_only_upstream_app_promo() {
    let app_promo = "Run `codex app` to open Codex Desktop";
    assert_eq!(WELCOME_TIP, replace_upstream_app_promo(app_promo));

    assert_eq!(
        WELCOME_TIP,
        replace_upstream_app_promo("Use the **desktop app** on Linux and run `chatgpt`.")
    );

    let unrelated_tip = "Use /fast for faster inference";
    assert_eq!(unrelated_tip, replace_upstream_app_promo(unrelated_tip));
}

fn render_welcome(show_dcg_nux: bool, height: u16) -> String {
    let mut terminal = Terminal::new(VT100Backend::new(/*width*/ 80, height)).expect("terminal");
    terminal
        .draw(|frame| {
            frame.render_widget(
                Text::from(welcome_help_lines_for(show_dcg_nux)),
                frame.area(),
            )
        })
        .expect("render welcome help");
    terminal.backend().to_string()
}

#[test]
fn welcome_help_first_and_later_startup_snapshot() {
    insta::assert_snapshot!(
        "codex_plus_plus_welcome_help",
        [
            render_welcome(/*show_dcg_nux*/ true, /*height*/ 5),
            render_welcome(/*show_dcg_nux*/ false, /*height*/ 3),
        ]
        .join("\n")
    );
}
