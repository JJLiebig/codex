use super::super::ContextualUserFragment;
use codex_protocol::models::ContentItemKind;

/// A bounded batch of runtime-observed process exits, without untrusted command output.
pub(crate) struct BackgroundCompletion(pub(crate) Vec<(i32, Option<i32>)>);

impl ContextualUserFragment for BackgroundCompletion {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("background_completion".into())
    }
    fn role(&self) -> &'static str {
        "user"
    }
    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }
    fn type_markers() -> (&'static str, &'static str) {
        ("<background_completion>", "</background_completion>")
    }
    fn body(&self) -> String {
        let mut text = String::from(
            "Background commands finished. Read output with write_stdin if needed, then continue the requested task.\n",
        );
        for (id, code) in self.0.iter().take(8) {
            let status = code.map_or_else(|| "unknown".to_string(), |code| code.to_string());
            text.push_str(&format!("session_id={id}, exit_code={status}\n"));
        }
        text
    }
}
