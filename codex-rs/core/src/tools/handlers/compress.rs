use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;
use serde::Deserialize;

pub struct CompressHandler;

#[derive(Debug, Deserialize)]
struct CompressArgs {
    #[serde(default)]
    topic: Option<String>,
    content: Vec<CompressRange>,
}

#[derive(Debug, Deserialize)]
struct CompressRange {
    #[serde(alias = "startId")]
    start_id: String,
    #[serde(alias = "endId")]
    end_id: String,
    summary: String,
}

impl ToolHandler for CompressHandler {
    type Output = FunctionToolOutput;

    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<Self::Output, FunctionCallError> {
        let arguments = match invocation.payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "compress handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: CompressArgs = parse_arguments(&arguments)?;
        validate_args(&args)?;
        let topic = args
            .topic
            .as_deref()
            .map(str::trim)
            .filter(|topic| !topic.is_empty())
            .unwrap_or("closed context");
        Ok(FunctionToolOutput::from_text(
            format!(
                "Compression request recorded for topic `{topic}` with {} range(s). CodeLink will apply it before the next model prompt if the message ids still match and are outside the recent-turn guard.",
                args.content.len()
            ),
            Some(true),
        ))
    }
}

fn validate_args(args: &CompressArgs) -> Result<(), FunctionCallError> {
    if args.content.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "compress requires at least one content range".to_string(),
        ));
    }
    for range in &args.content {
        if range.start_id.trim().is_empty() || range.end_id.trim().is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "compress range start_id and end_id must be non-empty".to_string(),
            ));
        }
        if range.summary.trim().is_empty() {
            return Err(FunctionCallError::RespondToModel(
                "compress range summary must be non-empty".to_string(),
            ));
        }
    }
    Ok(())
}
