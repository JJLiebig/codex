use crate::tools::context::ExecCommandToolOutput;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::ResponseInputItem;
use codex_tools::ToolOutput;
use codex_tools::ToolPayload;
use serde_json::Value;

const NOTICE: &str = "Completion will resume you automatically. Do independent work or end this turn. Do not sleep, wait, or poll for this session.";

pub(crate) struct WakeOutput(pub(crate) ExecCommandToolOutput);

impl ToolOutput for WakeOutput {
    fn log_output(&self) -> String {
        self.0.log_output()
    }
    fn success_for_logging(&self) -> bool {
        self.0.success_for_logging()
    }
    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        let mut item = self.0.to_response_item(call_id, payload);
        if let ResponseInputItem::FunctionCallOutput { output, .. }
        | ResponseInputItem::CustomToolCallOutput { output, .. } = &mut item
            && let FunctionCallOutputBody::Text(text) = &mut output.body
        {
            *text = format!("{NOTICE}\n{text}");
        }
        item
    }
    fn code_mode_result(&self, payload: &ToolPayload) -> Value {
        let mut result = self.0.code_mode_result(payload);
        result["completion_notice"] = Value::String(NOTICE.into());
        result
    }
    fn post_tool_use_id(&self, call_id: &str) -> String {
        self.0.post_tool_use_id(call_id)
    }
    fn post_tool_use_input(&self, payload: &ToolPayload) -> Option<Value> {
        self.0.post_tool_use_input(payload)
    }
    fn post_tool_use_response(&self, call_id: &str, payload: &ToolPayload) -> Option<Value> {
        self.0.post_tool_use_response(call_id, payload)
    }
}
