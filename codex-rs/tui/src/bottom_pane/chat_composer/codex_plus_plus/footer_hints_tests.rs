use super::*;
use super::super::TranscriptFooter;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use tokio::sync::mpsc::unbounded_channel;

fn composer() -> ChatComposer {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        AppEventSender::new(tx),
        /*enhanced_keys_supported*/ false,
        "Ask Codex".to_string(),
        /*disable_paste_burst*/ true,
    );
    composer.set_status_line_enabled(/*enabled*/ true);
    composer.set_status_line(Some(Line::from("MODEL STATUS")));
    composer
}

fn render(composer: &ChatComposer, width: u16, options: ComposerRenderOptions<'_>) -> Buffer {
    let area = Rect::new(0, 0, width, composer.desired_height_with_options(width, options));
    let mut buffer = Buffer::empty(area);
    composer.render_with_options(area, &mut buffer, /*mask_char*/ None, options);
    buffer
}

fn text(buffer: &Buffer) -> String {
    buffer
        .content
        .chunks(usize::from(buffer.area.width))
        .map(|row| row.iter().map(ratatui::buffer::Cell::symbol).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn hidden_footer_reclaims_row_and_preserves_status_and_draft() {
    for width in [20, 80, 120] {
        for draft in ["", "hello"] {
            let mut composer = composer();
            composer.draft.textarea.insert_str(draft);
            let shown = ComposerRenderOptions {
                separate_status_line: true,
                warning_count: 7,
                ..Default::default()
            };
            let hidden = ComposerRenderOptions {
                hide_footer_hints: true,
                ..shown
            };
            let visible_buffer = render(&composer, width, shown);
            let hidden_buffer = render(&composer, width, hidden);
            assert_eq!(hidden_buffer.area.height + 1, visible_buffer.area.height);
            assert_eq!(composer.footer_hint_height(width, hidden), 0);
            let hidden_text = text(&hidden_buffer);
            assert!(hidden_text.contains("MODEL STATUS"));
            assert!(hidden_text.contains(draft));
            assert!(!hidden_text.contains("shortcuts"));
            assert!(!hidden_text.contains('⚠'));
            assert!(composer.footer.warning_notice_area.get().is_none());
            assert!(composer.cursor_pos_with_options(hidden_buffer.area, hidden).is_some());
            assert_eq!(text(&render(&composer, width, shown)), text(&visible_buffer));
        }
    }
}

#[test]
fn default_and_native_scrollback_keep_existing_footer() {
    let composer = composer();
    let defaults = ComposerRenderOptions::default();
    assert!(!hide_passive_footer(&composer, defaults));
    let native = ComposerRenderOptions {
        hide_footer_hints: true,
        ..defaults
    };
    assert!(!hide_passive_footer(&composer, native));
    assert_eq!(render(&composer, 80, defaults), render(&composer, 80, native));
}

#[test]
fn navigation_and_interactive_search_keep_their_footer() {
    let composer = composer();
    for interactive in [false, true] {
        let footer = TranscriptFooter {
            text: Line::from("esc latest").into(),
            cursor_column: interactive.then_some(4),
            is_interactive: interactive,
        };
        let options = ComposerRenderOptions {
            separate_status_line: true,
            hide_footer_hints: true,
            footer: Some(&footer),
            ..Default::default()
        };
        assert!(!hide_passive_footer(&composer, options));
        assert!(composer.footer_hint_height(80, options) > 0);
    }
}

#[test]
fn explicit_help_custom_controls_and_feedback_remain_visible() {
    let mut composer = composer();
    let options = ComposerRenderOptions {
        separate_status_line: true,
        hide_footer_hints: true,
        ..Default::default()
    };
    composer.footer.mode = FooterMode::ShortcutOverlay;
    assert!(!hide_passive_footer(&composer, options));
    composer.footer.mode = FooterMode::ComposerEmpty;
    composer.set_footer_hint_override(Some(vec![("ctrl+c".into(), "cancel".into())]));
    assert!(!hide_passive_footer(&composer, options));
    composer.set_footer_hint_override(None);
    composer.show_footer_flash(
        Line::from("Copied selection"),
        std::time::Duration::from_secs(3),
    );
    assert!(!hide_passive_footer(&composer, options));
}

#[test]
fn clipped_hidden_footer_has_no_warning_hitbox() {
    let composer = composer();
    let options = ComposerRenderOptions {
        separate_status_line: true,
        hide_footer_hints: true,
        warning_count: 3,
        ..Default::default()
    };
    for width in [1, 8, 20, 80] {
        for height in [0, 1, 2, 3, 8] {
            let area = Rect::new(0, 0, width, height);
            let mut buffer = Buffer::empty(area);
            composer.render_with_options(area, &mut buffer, /*mask_char*/ None, options);
            assert!(composer.footer.warning_notice_area.get().is_none());
        }
    }
}
