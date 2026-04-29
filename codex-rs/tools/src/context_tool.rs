use crate::JsonSchema;
use crate::ResponsesApiTool;
use crate::ToolSpec;
use std::collections::BTreeMap;

pub const COMPRESS_TOOL_NAME: &str = "compress";

pub fn create_compress_tool() -> ToolSpec {
    let range_properties = BTreeMap::from([
        (
            "start_id".to_string(),
            JsonSchema::string(Some(
                "First visible CodeLink message id in the stale contiguous range, for example m0004."
                    .to_string(),
            )),
        ),
        (
            "end_id".to_string(),
            JsonSchema::string(Some(
                "Last visible CodeLink message id in the stale contiguous range, for example m0011."
                    .to_string(),
            )),
        ),
        (
            "summary".to_string(),
            JsonSchema::string(Some(
                "High-fidelity technical summary preserving decisions, file paths, commands, failures, and current state."
                    .to_string(),
            )),
        ),
    ]);

    let properties = BTreeMap::from([
        (
            "topic".to_string(),
            JsonSchema::string(Some(
                "Short label for the closed work being compressed.".to_string(),
            )),
        ),
        (
            "content".to_string(),
            JsonSchema::array(
                JsonSchema::object(
                    range_properties,
                    Some(vec![
                        "start_id".to_string(),
                        "end_id".to_string(),
                        "summary".to_string(),
                    ]),
                    Some(false.into()),
                ),
                Some("One or more stale contiguous message ranges to compress.".to_string()),
            ),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: COMPRESS_TOOL_NAME.to_string(),
        description:
            "Compresses closed, stale conversation ranges into durable technical summaries before future prompts are sent."
                .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["content".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}
