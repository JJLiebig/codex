use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;

use crate::context::BaseInstructionsFragment;
use crate::context::ContextualUserFragment;
use crate::context::ModelSwitchInstructions;
use crate::session::step_context::StepContext;

const PROGRESS_UPDATES_GUIDANCE: &str = "\n\nProgress updates: report meaningful findings, completed milestones, blockers, or decisions. Do not send updates or check status merely because time passed. When only waiting remains, prefer completion notifications or a long interruptible tool wait. For finite background commands, if the tool offers on_exit='wake', use it and finish the turn when no independent work remains; completion resumes the thread. Otherwise use a long interruptible tool wait until the command completes. When going idle, give one brief final handoff; do not add commentary repeating the same running status. Do not poll notified commands or delegate polling to an awaiter. For CI, prefer gh pr checks --watch or gh run watch so the command handles status checks without model calls. Keep the user informed when something changes, and answer user questions promptly.\n";

const MODEL_INSTRUCTIONS_KIND: &str = "model.instructions";
const MODEL_INSTRUCTIONS_OPEN: &str = "<model_instructions>";
const MODEL_INSTRUCTIONS_PREFIX: &str =
    "The following instructions replace the previous base instructions:\n\n";
const MODEL_INSTRUCTIONS_SUFFIX: &str = "\n</model_instructions>";

const MODEL_SWITCH_KIND: &str = "model_switch.instructions";
const MODEL_SWITCH_PREFIX: &str = "<model_switch>\nThe user was previously using a different model. Please continue the conversation according to the following instructions:\n\n";
const MODEL_SWITCH_SUFFIX: &str = "\n</model_switch>";

/// Adapt only the request copy; preserve stored instructions and custom user guidance.
pub(crate) fn instructions(text: &str) -> String {
    let mut text = text.to_owned();
    for sentence in [
        "Avoid performing blocking sleep or wait calls longer than 60 seconds, as they may prevent you from communicating with the user for their duration.",
        "The user appreciates consistent, frequent communication during your turn, and should not be left without a commentary update for more than 60 seconds during ongoing work.",
    ] {
        text = text
            .replace(&format!("- {sentence}"), "")
            .replace(sentence, "");
    }
    for sentence in [
        "Prefer sending consistent, frequent communication to the user during your turn.",
        "When the user asks you to wait or monitor through completion or if it's needed for a longer horizon task, you can sleep for as long as is required (generally 60 seconds by default) between checks.",
    ] {
        text = text
            .replace(&format!(" {sentence}"), "")
            .replace(sentence, "");
    }
    if !text.ends_with(PROGRESS_UPDATES_GUIDANCE) {
        text.push_str(PROGRESS_UPDATES_GUIDANCE);
    }
    text
}

/// Adapt request-only harness-classified base and model-switch instruction items without mutating
/// persisted history.
pub(crate) fn adapt_input(
    mut input: Vec<ResponseItem>,
    step_context: &StepContext,
) -> Vec<ResponseItem> {
    if crate::guardian::is_basic_session_source(&step_context.turn.session_source)
        || !codex_config::disable_unnecessary_updates(&step_context.turn.config.config_layer_stack)
    {
        return input;
    }

    for item in &mut input {
        let ResponseItem::Message {
            role,
            content,
            internal_chat_message_metadata_passthrough,
            ..
        } = item
        else {
            continue;
        };
        if role != "developer" {
            continue;
        }
        let [ContentItem::InputText { text }] = content.as_mut_slice() else {
            continue;
        };
        let kind = internal_chat_message_metadata_passthrough
            .as_ref()
            .and_then(|metadata| metadata.content_item_kinds.as_deref())
            .and_then(|kinds| match kinds {
                [kind] => Some(kind.as_str()),
                _ => None,
            });
        match kind {
            Some(BaseInstructionsFragment::KIND) => {
                if !text.is_empty() {
                    *text = instructions(text);
                }
            }
            Some(MODEL_INSTRUCTIONS_KIND) => {
                if text.starts_with(MODEL_INSTRUCTIONS_OPEN)
                    && text.ends_with(MODEL_INSTRUCTIONS_SUFFIX)
                    && let Some(prefix_start) = text.find(MODEL_INSTRUCTIONS_PREFIX)
                {
                    let body_start = prefix_start + MODEL_INSTRUCTIONS_PREFIX.len();
                    let body_end = text.len() - MODEL_INSTRUCTIONS_SUFFIX.len();
                    let body = &text[body_start..body_end];
                    if !body.is_empty() {
                        let adapted = instructions(body);
                        *text = format!(
                            "{}{adapted}{MODEL_INSTRUCTIONS_SUFFIX}",
                            &text[..body_start]
                        );
                    }
                }
            }
            Some(MODEL_SWITCH_KIND) => adapt_model_switch_text(text),
            None if ModelSwitchInstructions::matches_text(text) => adapt_model_switch_text(text),
            _ => {}
        }
    }

    input
}

fn adapt_model_switch_text(text: &mut String) {
    if let Some(body) = text
        .strip_prefix(MODEL_SWITCH_PREFIX)
        .and_then(|rest| rest.strip_suffix(MODEL_SWITCH_SUFFIX))
        && !body.is_empty()
    {
        *text = format!(
            "{MODEL_SWITCH_PREFIX}{}{MODEL_SWITCH_SUFFIX}",
            instructions(body)
        );
    }
}
