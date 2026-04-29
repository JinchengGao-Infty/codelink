use super::history::is_user_turn_boundary;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use std::collections::HashMap;
use std::collections::HashSet;

const ENABLE_ENV: &str = "CODELINK_CONTEXT_PRUNER";
const DIRECTIVES_ENV: &str = "CODELINK_CONTEXT_DIRECTIVES";
const KEEP_RECENT_TURNS_ENV: &str = "CODELINK_PRUNE_KEEP_RECENT_TURNS";
const SEGMENT_TURNS_ENV: &str = "CODELINK_PRUNE_SEGMENT_TURNS";
const HEAVY_TOOL_CHARS_ENV: &str = "CODELINK_PRUNE_HEAVY_TOOL_CHARS";
const LEGACY_ENABLE_ENV: &str = "CODEX_MANGA_CONTEXT_PRUNER";
const LEGACY_DIRECTIVES_ENV: &str = "CODEX_MANGA_CONTEXT_DIRECTIVES";
const LEGACY_KEEP_RECENT_TURNS_ENV: &str = "CODEX_MANGA_PRUNE_KEEP_RECENT_TURNS";
const LEGACY_SEGMENT_TURNS_ENV: &str = "CODEX_MANGA_PRUNE_SEGMENT_TURNS";
const LEGACY_HEAVY_TOOL_CHARS_ENV: &str = "CODEX_MANGA_PRUNE_HEAVY_TOOL_CHARS";
const DEFAULT_KEEP_RECENT_TURNS: usize = 1;
const DEFAULT_SEGMENT_TURNS: usize = 10;
const DEFAULT_HEAVY_TOOL_CHARS: usize = 4096;
const CHECKPOINT_PREFIX: &str = "[codelink-context-checkpoint ";
const LEGACY_CHECKPOINT_PREFIX: &str = "[manga-context-checkpoint ";
const CHECKPOINT_SCOPE_OLDER_IMAGES: &str = "scope=older-images";
const CHECKPOINT_SCOPE_OLDER_HEAVY: &str = "scope=older-heavy";
const LEGACY_TURNS_PREFIX: &str = "turns=";
const CHECKPOINT_APPLIED_PLACEHOLDER: &str = "[codelink-context-checkpoint applied]";
const MAX_CHECKPOINT_SUMMARY_CHARS: usize = 800;
const IMAGE_PLACEHOLDER: &str = "[codelink-context-pruned: image payload removed from older history; use local files, visual QA verdicts, or continuity ledger if needed]";
const DUPLICATE_TOOL_OUTPUT_PLACEHOLDER: &str =
    "[codelink-context-pruned: duplicate tool output removed; newer identical call is preserved]";
const ERROR_TOOL_INPUT_PLACEHOLDER: &str =
    "[codelink-context-pruned: failed tool call input removed; error output is preserved]";
const ERROR_TOOL_OUTPUT_PLACEHOLDER: &str =
    "[codelink-context-pruned: failed tool output removed from older history]";
const HEAVY_TOOL_INPUT_PREFIX: &str = "[codelink-context-pruned: old heavy tool input removed";
const HEAVY_TOOL_OUTPUT_PREFIX: &str = "[codelink-context-pruned: old heavy tool output removed";
const CONTEXT_PRUNER_DIRECTIVE_INSTRUCTIONS: &str = r#"CodeLink manga profile context pruning:
- You may request runtime context pruning after a stable manga checkpoint, usually every 10 pages or after visual QA has produced text verdicts.
- Emit exactly one checkpoint line when older image payloads are no longer needed:
  [codelink-context-checkpoint scope=older-images] brief continuity summary and QA status
- Emit a heavy checkpoint when older tool/search/patch payloads are no longer needed:
  [codelink-context-checkpoint scope=older-heavy] brief tool/history summary and current state
- The runtime will automatically find older image/tool payloads, preserve user text, assistant verdicts, and keep the recent turn guarded.
- Do not emit checkpoint lines casually; frequent mid-history pruning hurts prompt-cache reuse."#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ContextPrunerConfig {
    keep_recent_turns: usize,
    segment_turns: usize,
    heavy_tool_chars: usize,
    enable_directives: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckpointDirective {
    target: CheckpointTarget,
    summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CheckpointTarget {
    OlderImages,
    OlderHeavy,
    LegacyTurns(usize),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolCallMetadata {
    name: String,
    input_preview: String,
    input_chars: usize,
}

impl ContextPrunerConfig {
    fn from_env() -> Option<Self> {
        if !env_truthy_any(&[ENABLE_ENV, LEGACY_ENABLE_ENV]) {
            return None;
        }

        Some(Self {
            keep_recent_turns: env_var_any(&[KEEP_RECENT_TURNS_ENV, LEGACY_KEEP_RECENT_TURNS_ENV])
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(DEFAULT_KEEP_RECENT_TURNS),
            segment_turns: env_var_any(&[SEGMENT_TURNS_ENV, LEGACY_SEGMENT_TURNS_ENV])
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(DEFAULT_SEGMENT_TURNS),
            heavy_tool_chars: env_var_any(&[HEAVY_TOOL_CHARS_ENV, LEGACY_HEAVY_TOOL_CHARS_ENV])
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(DEFAULT_HEAVY_TOOL_CHARS),
            enable_directives: env_var_any(&[DIRECTIVES_ENV, LEGACY_DIRECTIVES_ENV])
                .map(|value| env_value_truthy(&value))
                .unwrap_or(true),
        })
    }
}

pub(crate) fn developer_instructions_from_env() -> Option<String> {
    let config = ContextPrunerConfig::from_env()?;
    if config.enable_directives {
        Some(CONTEXT_PRUNER_DIRECTIVE_INSTRUCTIONS.to_string())
    } else {
        None
    }
}

pub(crate) fn prune_items_for_prompt_from_env(items: &mut [ResponseItem]) {
    if let Some(config) = ContextPrunerConfig::from_env() {
        prune_items_for_prompt(items, config);
    }
}

fn prune_items_for_prompt(items: &mut [ResponseItem], config: ContextPrunerConfig) {
    let checkpoint = if config.enable_directives {
        latest_checkpoint_directive(items)
    } else {
        None
    };
    if config.enable_directives {
        strip_checkpoint_directives_from_prompt(items);
    }

    let automatic_cutoff = pruning_cutoff(items, config.keep_recent_turns, config.segment_turns);
    let (checkpoint_image_cutoff, checkpoint_heavy_cutoff) = checkpoint
        .as_ref()
        .map(|checkpoint| checkpoint_cutoffs(items, checkpoint, &config))
        .unwrap_or((0, 0));
    let image_cutoff = automatic_cutoff.max(checkpoint_image_cutoff);
    let heavy_cutoff = automatic_cutoff.max(checkpoint_heavy_cutoff);
    let maintenance_cutoff = image_cutoff.max(heavy_cutoff);
    if maintenance_cutoff == 0 {
        return;
    }

    let image_placeholder = image_placeholder(checkpoint.as_ref());
    prune_duplicate_tool_outputs(items, maintenance_cutoff);
    purge_old_failed_tool_payloads(items, maintenance_cutoff);
    prune_heavy_tool_payloads(items, heavy_cutoff, config.heavy_tool_chars);
    for item in &mut items[..image_cutoff] {
        prune_item_images(item, &image_placeholder);
    }
}

fn pruning_cutoff(items: &[ResponseItem], keep_recent_turns: usize, segment_turns: usize) -> usize {
    let mut turn_positions = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if is_user_turn_boundary(item) {
            turn_positions.push(index);
        }
    }

    let eligible_turns = turn_positions.len().saturating_sub(keep_recent_turns);
    let segment_turns = segment_turns.max(1);
    let pruned_turns = eligible_turns / segment_turns * segment_turns;
    if pruned_turns == 0 {
        0
    } else if pruned_turns >= turn_positions.len() {
        items.len()
    } else {
        turn_positions[pruned_turns]
    }
}

fn checkpoint_cutoffs(
    items: &[ResponseItem],
    checkpoint: &CheckpointDirective,
    config: &ContextPrunerConfig,
) -> (usize, usize) {
    match checkpoint.target {
        CheckpointTarget::OlderImages => {
            let cutoff = pruning_cutoff_after_latest_prunable_image(
                items,
                recent_turn_guard_index(items, config.keep_recent_turns),
            );
            (cutoff, 0)
        }
        CheckpointTarget::OlderHeavy => {
            let cutoff = pruning_cutoff_after_latest_prunable_heavy_tool(
                items,
                recent_turn_guard_index(items, config.keep_recent_turns),
                config.heavy_tool_chars,
            );
            (0, cutoff)
        }
        CheckpointTarget::LegacyTurns(requested_turns) => {
            let cutoff = pruning_cutoff_for_requested_turns(
                items,
                requested_turns,
                config.keep_recent_turns,
            );
            (cutoff, cutoff)
        }
    }
}

fn pruning_cutoff_for_requested_turns(
    items: &[ResponseItem],
    requested_turns: usize,
    keep_recent_turns: usize,
) -> usize {
    let mut turn_positions = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if is_user_turn_boundary(item) {
            turn_positions.push(index);
        }
    }

    let pruned_turns = requested_turns.min(turn_positions.len().saturating_sub(keep_recent_turns));
    if pruned_turns == 0 {
        0
    } else if pruned_turns >= turn_positions.len() {
        items.len()
    } else {
        turn_positions[pruned_turns]
    }
}

fn pruning_cutoff_after_latest_prunable_image(
    items: &[ResponseItem],
    recent_turn_guard_index: usize,
) -> usize {
    items
        .iter()
        .enumerate()
        .take(recent_turn_guard_index)
        .filter(|(_, item)| item_has_prunable_image(item))
        .map(|(index, _)| index.saturating_add(1))
        .last()
        .unwrap_or(0)
}

fn pruning_cutoff_after_latest_prunable_heavy_tool(
    items: &[ResponseItem],
    recent_turn_guard_index: usize,
    heavy_tool_chars: usize,
) -> usize {
    items
        .iter()
        .enumerate()
        .take(recent_turn_guard_index)
        .filter(|(_, item)| item_has_prunable_heavy_tool_payload(item, heavy_tool_chars))
        .map(|(index, _)| index.saturating_add(1))
        .last()
        .unwrap_or(0)
}

fn recent_turn_guard_index(items: &[ResponseItem], keep_recent_turns: usize) -> usize {
    let mut turn_positions = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if is_user_turn_boundary(item) {
            turn_positions.push(index);
        }
    }

    if keep_recent_turns == 0 {
        items.len()
    } else if keep_recent_turns >= turn_positions.len() {
        0
    } else {
        turn_positions[turn_positions.len() - keep_recent_turns]
    }
}

fn item_has_prunable_image(item: &ResponseItem) -> bool {
    match item {
        ResponseItem::Message { content, .. } => content
            .iter()
            .any(|content_item| matches!(content_item, ContentItem::InputImage { .. })),
        ResponseItem::FunctionCallOutput { output, .. }
        | ResponseItem::CustomToolCallOutput { output, .. } => output
            .content_items()
            .map(|content_items| {
                content_items.iter().any(|content_item| {
                    matches!(
                        content_item,
                        FunctionCallOutputContentItem::InputImage { .. }
                    )
                })
            })
            .unwrap_or(false),
        ResponseItem::ImageGenerationCall { result, .. } => !result.is_empty(),
        ResponseItem::Reasoning { .. }
        | ResponseItem::LocalShellCall { .. }
        | ResponseItem::FunctionCall { .. }
        | ResponseItem::ToolSearchCall { .. }
        | ResponseItem::ToolSearchOutput { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::CustomToolCall { .. }
        | ResponseItem::Compaction { .. }
        | ResponseItem::Other => false,
    }
}

fn item_has_prunable_heavy_tool_payload(item: &ResponseItem, heavy_tool_chars: usize) -> bool {
    match item {
        ResponseItem::FunctionCall {
            name, arguments, ..
        }
        | ResponseItem::CustomToolCall {
            name,
            input: arguments,
            ..
        } => !is_heavy_tool_prune_protected(name) && arguments.chars().count() > heavy_tool_chars,
        ResponseItem::FunctionCallOutput { output, .. }
        | ResponseItem::CustomToolCallOutput { output, .. } => output_text_len(output)
            .map(|chars| chars > heavy_tool_chars)
            .unwrap_or(false),
        ResponseItem::Message { .. }
        | ResponseItem::Reasoning { .. }
        | ResponseItem::LocalShellCall { .. }
        | ResponseItem::ToolSearchCall { .. }
        | ResponseItem::ToolSearchOutput { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::ImageGenerationCall { .. }
        | ResponseItem::Compaction { .. }
        | ResponseItem::Other => false,
    }
}

fn prune_item_images(item: &mut ResponseItem, image_placeholder: &str) {
    match item {
        ResponseItem::Message { content, .. } => {
            for content_item in content {
                if matches!(content_item, ContentItem::InputImage { .. }) {
                    *content_item = ContentItem::InputText {
                        text: image_placeholder.to_string(),
                    };
                }
            }
        }
        ResponseItem::FunctionCallOutput { output, .. }
        | ResponseItem::CustomToolCallOutput { output, .. } => {
            prune_function_output_images(output, image_placeholder);
        }
        ResponseItem::ImageGenerationCall { result, .. } => {
            if !result.is_empty() {
                *result = image_placeholder.to_string();
            }
        }
        ResponseItem::Reasoning { .. }
        | ResponseItem::LocalShellCall { .. }
        | ResponseItem::FunctionCall { .. }
        | ResponseItem::ToolSearchCall { .. }
        | ResponseItem::ToolSearchOutput { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::CustomToolCall { .. }
        | ResponseItem::Compaction { .. }
        | ResponseItem::Other => {}
    }
}

fn prune_function_output_images(output: &mut FunctionCallOutputPayload, image_placeholder: &str) {
    let Some(content_items) = output.content_items_mut() else {
        return;
    };

    for content_item in content_items {
        if matches!(
            content_item,
            FunctionCallOutputContentItem::InputImage { .. }
        ) {
            *content_item = FunctionCallOutputContentItem::InputText {
                text: image_placeholder.to_string(),
            };
        }
    }
}

fn latest_checkpoint_directive(items: &[ResponseItem]) -> Option<CheckpointDirective> {
    items
        .iter()
        .filter_map(message_content_items)
        .flat_map(|content| content.iter())
        .filter_map(content_item_text)
        .filter_map(extract_checkpoint_directive)
        .last()
}

fn strip_checkpoint_directives_from_prompt(items: &mut [ResponseItem]) {
    for item in items {
        let Some(content) = message_content_items_mut(item) else {
            continue;
        };
        for content_item in content {
            let Some(text) = content_item_text_mut(content_item) else {
                continue;
            };
            *text = strip_checkpoint_directive_lines(text);
        }
    }
}

fn message_content_items(item: &ResponseItem) -> Option<&Vec<ContentItem>> {
    match item {
        ResponseItem::Message { content, .. } => Some(content),
        _ => None,
    }
}

fn message_content_items_mut(item: &mut ResponseItem) -> Option<&mut Vec<ContentItem>> {
    match item {
        ResponseItem::Message { content, .. } => Some(content),
        _ => None,
    }
}

fn content_item_text(item: &ContentItem) -> Option<&str> {
    match item {
        ContentItem::InputText { text } | ContentItem::OutputText { text } => Some(text.as_str()),
        ContentItem::InputImage { .. } => None,
    }
}

fn content_item_text_mut(item: &mut ContentItem) -> Option<&mut String> {
    match item {
        ContentItem::InputText { text } | ContentItem::OutputText { text } => Some(text),
        ContentItem::InputImage { .. } => None,
    }
}

fn extract_checkpoint_directive(text: &str) -> Option<CheckpointDirective> {
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = strip_checkpoint_prefix(line) else {
            continue;
        };
        let Some((directive, summary)) = rest.split_once(']') else {
            continue;
        };
        let Some(target) = parse_checkpoint_target(directive.trim()) else {
            continue;
        };
        return Some(CheckpointDirective {
            target,
            summary: trim_checkpoint_summary(summary),
        });
    }
    None
}

fn parse_checkpoint_target(directive: &str) -> Option<CheckpointTarget> {
    if directive == CHECKPOINT_SCOPE_OLDER_IMAGES {
        return Some(CheckpointTarget::OlderImages);
    }
    if directive == CHECKPOINT_SCOPE_OLDER_HEAVY {
        return Some(CheckpointTarget::OlderHeavy);
    }

    let turns_text = directive.strip_prefix(LEGACY_TURNS_PREFIX)?;
    turns_text
        .trim()
        .parse::<usize>()
        .ok()
        .map(CheckpointTarget::LegacyTurns)
}

fn strip_checkpoint_directive_lines(text: &str) -> String {
    let kept_lines = text
        .lines()
        .filter(|line| strip_checkpoint_prefix(line.trim_start()).is_none())
        .collect::<Vec<_>>();

    if kept_lines.is_empty() {
        CHECKPOINT_APPLIED_PLACEHOLDER.to_string()
    } else {
        kept_lines.join("\n")
    }
}

fn trim_checkpoint_summary(summary: &str) -> Option<String> {
    let summary = summary.trim();
    if summary.is_empty() {
        return None;
    }

    let mut output = String::new();
    for ch in summary.chars().take(MAX_CHECKPOINT_SUMMARY_CHARS) {
        output.push(ch);
    }
    Some(output)
}

fn image_placeholder(checkpoint: Option<&CheckpointDirective>) -> String {
    match checkpoint.and_then(|checkpoint| checkpoint.summary.as_deref()) {
        Some(summary) => format!("{IMAGE_PLACEHOLDER}; checkpoint summary: {summary}"),
        None => IMAGE_PLACEHOLDER.to_string(),
    }
}

fn prune_duplicate_tool_outputs(items: &mut [ResponseItem], cutoff: usize) {
    let mut newest_call_by_signature: HashMap<String, String> = HashMap::new();
    let mut duplicate_call_ids = HashSet::new();

    for item in items.iter() {
        let Some((call_id, signature)) = tool_call_signature(item) else {
            continue;
        };

        if let Some(previous_call_id) = newest_call_by_signature.insert(signature, call_id.clone())
        {
            duplicate_call_ids.insert(previous_call_id);
        }
    }

    if duplicate_call_ids.is_empty() {
        return;
    }

    for item in &mut items[..cutoff] {
        match item {
            ResponseItem::FunctionCallOutput { call_id, output }
            | ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } if duplicate_call_ids.contains(call_id) => {
                *output =
                    FunctionCallOutputPayload::from_text(DUPLICATE_TOOL_OUTPUT_PLACEHOLDER.into());
            }
            _ => {}
        }
    }
}

fn purge_old_failed_tool_payloads(items: &mut [ResponseItem], cutoff: usize) {
    let failed_call_ids = items[..cutoff]
        .iter()
        .filter_map(failed_tool_output_call_id)
        .collect::<HashSet<_>>();

    if failed_call_ids.is_empty() {
        return;
    }

    for item in &mut items[..cutoff] {
        match item {
            ResponseItem::FunctionCall {
                call_id, arguments, ..
            }
            | ResponseItem::CustomToolCall {
                call_id,
                input: arguments,
                ..
            } if failed_call_ids.contains(call_id) => {
                *arguments = ERROR_TOOL_INPUT_PLACEHOLDER.to_string();
            }
            ResponseItem::FunctionCallOutput { call_id, output }
            | ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } if failed_call_ids.contains(call_id) => {
                *output =
                    FunctionCallOutputPayload::from_text(ERROR_TOOL_OUTPUT_PLACEHOLDER.into());
            }
            _ => {}
        }
    }
}

fn prune_heavy_tool_payloads(items: &mut [ResponseItem], cutoff: usize, heavy_tool_chars: usize) {
    if cutoff == 0 {
        return;
    }

    let metadata_by_call_id = tool_call_metadata_by_id(items);
    for item in &mut items[..cutoff] {
        match item {
            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            }
            | ResponseItem::CustomToolCall {
                name,
                input: arguments,
                call_id,
                ..
            } => {
                if is_heavy_tool_prune_protected(name) {
                    continue;
                }
                let input_chars = arguments.chars().count();
                if input_chars <= heavy_tool_chars {
                    continue;
                }
                *arguments = heavy_tool_input_placeholder(name, call_id, input_chars, arguments);
            }
            ResponseItem::FunctionCallOutput { call_id, output } => {
                prune_heavy_tool_output(
                    call_id,
                    None,
                    output,
                    &metadata_by_call_id,
                    heavy_tool_chars,
                );
            }
            ResponseItem::CustomToolCallOutput {
                call_id,
                name,
                output,
            } => {
                prune_heavy_tool_output(
                    call_id,
                    name.as_deref(),
                    output,
                    &metadata_by_call_id,
                    heavy_tool_chars,
                );
            }
            ResponseItem::Message { .. }
            | ResponseItem::Reasoning { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::ToolSearchOutput { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::Compaction { .. }
            | ResponseItem::Other => {}
        }
    }
}

fn prune_heavy_tool_output(
    call_id: &str,
    output_name: Option<&str>,
    output: &mut FunctionCallOutputPayload,
    metadata_by_call_id: &HashMap<String, ToolCallMetadata>,
    heavy_tool_chars: usize,
) {
    let Some(output_text) = output.body.to_text() else {
        return;
    };
    let output_chars = output_text.chars().count();
    if output_chars <= heavy_tool_chars {
        return;
    }

    let metadata = metadata_by_call_id.get(call_id);
    let tool_name = output_name
        .or_else(|| metadata.map(|metadata| metadata.name.as_str()))
        .unwrap_or("unknown");
    if is_heavy_tool_prune_protected(tool_name) {
        return;
    }

    let success = output.success;
    let placeholder = heavy_tool_output_placeholder(
        tool_name,
        call_id,
        output_chars,
        success,
        &output_text,
        metadata,
    );
    *output = FunctionCallOutputPayload::from_text(placeholder);
}

fn tool_call_metadata_by_id(items: &[ResponseItem]) -> HashMap<String, ToolCallMetadata> {
    let mut metadata_by_call_id = HashMap::new();
    for item in items {
        match item {
            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            }
            | ResponseItem::CustomToolCall {
                name,
                input: arguments,
                call_id,
                ..
            } => {
                metadata_by_call_id.insert(
                    call_id.clone(),
                    ToolCallMetadata {
                        name: name.clone(),
                        input_preview: preview(arguments),
                        input_chars: arguments.chars().count(),
                    },
                );
            }
            ResponseItem::Message { .. }
            | ResponseItem::Reasoning { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::ToolSearchOutput { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::FunctionCallOutput { .. }
            | ResponseItem::CustomToolCallOutput { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::Compaction { .. }
            | ResponseItem::Other => {}
        }
    }
    metadata_by_call_id
}

fn heavy_tool_input_placeholder(
    tool_name: &str,
    call_id: &str,
    input_chars: usize,
    input: &str,
) -> String {
    format!(
        "{HEAVY_TOOL_INPUT_PREFIX}; tool={tool_name}; call_id={call_id}; chars={input_chars}; preview=\"{}\"]",
        preview(input)
    )
}

fn heavy_tool_output_placeholder(
    tool_name: &str,
    call_id: &str,
    output_chars: usize,
    success: Option<bool>,
    output: &str,
    metadata: Option<&ToolCallMetadata>,
) -> String {
    let success_text = match success {
        Some(true) => "true",
        Some(false) => "false",
        None => "unknown",
    };
    let exit_code_text = extract_exit_code(output)
        .map(|exit_code| format!("; exit_code={exit_code}"))
        .unwrap_or_default();
    let input_text = metadata
        .map(|metadata| {
            format!(
                "; input_chars={}; input_preview=\"{}\"",
                metadata.input_chars, metadata.input_preview
            )
        })
        .unwrap_or_default();
    format!(
        "{HEAVY_TOOL_OUTPUT_PREFIX}; tool={tool_name}; call_id={call_id}; chars={output_chars}; success={success_text}{exit_code_text}{input_text}; preview=\"{}\"]",
        preview(output)
    )
}

fn tool_call_signature(item: &ResponseItem) -> Option<(String, String)> {
    match item {
        ResponseItem::FunctionCall {
            name,
            namespace,
            arguments,
            call_id,
            ..
        } if !is_protected_tool(name) => Some((
            call_id.clone(),
            format!(
                "function:{}:{}:{arguments}",
                namespace.as_deref().unwrap_or_default(),
                name
            ),
        )),
        ResponseItem::CustomToolCall {
            name,
            input,
            call_id,
            ..
        } if !is_protected_tool(name) => Some((call_id.clone(), format!("custom:{name}:{input}"))),
        _ => None,
    }
}

fn output_text_len(output: &FunctionCallOutputPayload) -> Option<usize> {
    output.body.to_text().map(|text| text.chars().count())
}

fn extract_exit_code(output: &str) -> Option<i32> {
    let marker = "Process exited with code ";
    let start = output.find(marker)? + marker.len();
    let code_text = output[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '-')
        .collect::<String>();
    code_text.parse::<i32>().ok()
}

fn preview(text: &str) -> String {
    let mut output = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        let normalized = if ch.is_whitespace() { ' ' } else { ch };
        if normalized == ' ' && output.ends_with(' ') {
            continue;
        }
        output.push(normalized);
        if output.chars().count() >= 160 {
            if chars.peek().is_some() {
                output.push('…');
            }
            break;
        }
    }
    output.replace('"', "'")
}

fn failed_tool_output_call_id(item: &ResponseItem) -> Option<String> {
    match item {
        ResponseItem::FunctionCallOutput { call_id, output }
        | ResponseItem::CustomToolCallOutput {
            call_id, output, ..
        } if output.success == Some(false) => Some(call_id.clone()),
        _ => None,
    }
}

fn is_heavy_tool_prune_protected(name: &str) -> bool {
    matches!(
        name,
        "task"
            | "skill"
            | "todowrite"
            | "todoread"
            | "compress"
            | "batch"
            | "plan_enter"
            | "plan_exit"
    )
}

fn is_protected_tool(name: &str) -> bool {
    matches!(
        name,
        "apply_patch"
            | "edit"
            | "write"
            | "task"
            | "skill"
            | "todowrite"
            | "todoread"
            | "compress"
            | "batch"
            | "plan_enter"
            | "plan_exit"
    )
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|value| env_value_truthy(&value))
        .unwrap_or(false)
}

fn env_truthy_any(names: &[&str]) -> bool {
    names.iter().any(|name| env_truthy(name))
}

fn env_var_any(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| std::env::var(name).ok())
}

fn env_value_truthy(value: &str) -> bool {
    matches!(value, "1" | "true" | "TRUE" | "yes" | "on")
}

fn strip_checkpoint_prefix(line: &str) -> Option<&str> {
    line.strip_prefix(CHECKPOINT_PREFIX)
        .or_else(|| line.strip_prefix(LEGACY_CHECKPOINT_PREFIX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_protocol::models::FunctionCallOutputPayload;
    use pretty_assertions::assert_eq;

    fn user_text(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: text.to_string(),
            }],
            phase: None,
        }
    }

    fn user_image(image_url: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputImage {
                image_url: image_url.to_string(),
                detail: None,
            }],
            phase: None,
        }
    }

    fn assistant_text(text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: text.to_string(),
            }],
            phase: None,
        }
    }

    fn config(keep_recent_turns: usize, segment_turns: usize) -> ContextPrunerConfig {
        ContextPrunerConfig {
            keep_recent_turns,
            segment_turns,
            heavy_tool_chars: DEFAULT_HEAVY_TOOL_CHARS,
            enable_directives: true,
        }
    }

    fn config_with_heavy(
        keep_recent_turns: usize,
        segment_turns: usize,
        heavy_tool_chars: usize,
    ) -> ContextPrunerConfig {
        ContextPrunerConfig {
            keep_recent_turns,
            segment_turns,
            heavy_tool_chars,
            enable_directives: true,
        }
    }

    #[test]
    fn prunes_images_before_recent_turn_boundary() {
        let mut items = vec![
            user_image("data:image/png;base64,old"),
            ResponseItem::ImageGenerationCall {
                id: "ig-old".to_string(),
                status: "completed".to_string(),
                revised_prompt: None,
                result: "old-result".to_string(),
            },
            user_text("recent"),
            user_image("data:image/png;base64,recent"),
        ];

        prune_items_for_prompt(&mut items, config(1, 1));

        assert_eq!(
            items,
            vec![
                ResponseItem::Message {
                    id: None,
                    role: "user".to_string(),
                    content: vec![ContentItem::InputText {
                        text: IMAGE_PLACEHOLDER.to_string(),
                    }],
                    phase: None,
                },
                ResponseItem::ImageGenerationCall {
                    id: "ig-old".to_string(),
                    status: "completed".to_string(),
                    revised_prompt: None,
                    result: IMAGE_PLACEHOLDER.to_string(),
                },
                user_text("recent"),
                user_image("data:image/png;base64,recent"),
            ]
        );
    }

    #[test]
    fn prunes_tool_output_images_without_touching_text() {
        let mut items = vec![
            user_text("old"),
            ResponseItem::FunctionCallOutput {
                call_id: "call-old".to_string(),
                output: FunctionCallOutputPayload::from_content_items(vec![
                    FunctionCallOutputContentItem::InputText {
                        text: "keep text".to_string(),
                    },
                    FunctionCallOutputContentItem::InputImage {
                        image_url: "data:image/png;base64,old".to_string(),
                        detail: None,
                    },
                ]),
            },
            user_text("recent"),
        ];

        prune_items_for_prompt(&mut items, config(1, 1));

        assert_eq!(
            items,
            vec![
                user_text("old"),
                ResponseItem::FunctionCallOutput {
                    call_id: "call-old".to_string(),
                    output: FunctionCallOutputPayload::from_content_items(vec![
                        FunctionCallOutputContentItem::InputText {
                            text: "keep text".to_string(),
                        },
                        FunctionCallOutputContentItem::InputText {
                            text: IMAGE_PLACEHOLDER.to_string(),
                        },
                    ]),
                },
                user_text("recent"),
            ]
        );
    }

    #[test]
    fn keeps_all_images_when_history_has_only_recent_turns() {
        let mut items = vec![user_image("data:image/png;base64,only")];

        prune_items_for_prompt(&mut items, config(1, 1));

        assert_eq!(items, vec![user_image("data:image/png;base64,only")]);
    }

    #[test]
    fn segment_cutoff_prunes_only_at_stable_boundaries() {
        let mut items = vec![
            user_image("data:image/png;base64,turn1"),
            user_image("data:image/png;base64,turn2"),
            user_image("data:image/png;base64,turn3"),
            user_image("data:image/png;base64,turn4"),
        ];

        prune_items_for_prompt(&mut items, config(1, 3));

        assert_eq!(
            items,
            vec![
                ResponseItem::Message {
                    id: None,
                    role: "user".to_string(),
                    content: vec![ContentItem::InputText {
                        text: IMAGE_PLACEHOLDER.to_string(),
                    }],
                    phase: None,
                },
                ResponseItem::Message {
                    id: None,
                    role: "user".to_string(),
                    content: vec![ContentItem::InputText {
                        text: IMAGE_PLACEHOLDER.to_string(),
                    }],
                    phase: None,
                },
                ResponseItem::Message {
                    id: None,
                    role: "user".to_string(),
                    content: vec![ContentItem::InputText {
                        text: IMAGE_PLACEHOLDER.to_string(),
                    }],
                    phase: None,
                },
                user_image("data:image/png;base64,turn4"),
            ]
        );
    }

    #[test]
    fn prunes_duplicate_tool_outputs_like_dcp() {
        let mut items = vec![
            user_text("old"),
            ResponseItem::FunctionCall {
                id: None,
                name: "view_image".to_string(),
                namespace: None,
                arguments: "{\"path\":\"a.png\"}".to_string(),
                call_id: "call-older".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "call-older".to_string(),
                output: FunctionCallOutputPayload::from_text("older output".to_string()),
            },
            ResponseItem::FunctionCall {
                id: None,
                name: "view_image".to_string(),
                namespace: None,
                arguments: "{\"path\":\"a.png\"}".to_string(),
                call_id: "call-newer".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "call-newer".to_string(),
                output: FunctionCallOutputPayload::from_text("newer output".to_string()),
            },
            user_text("recent"),
        ];

        prune_items_for_prompt(&mut items, config(1, 1));

        assert_eq!(
            items[2],
            ResponseItem::FunctionCallOutput {
                call_id: "call-older".to_string(),
                output: FunctionCallOutputPayload::from_text(
                    DUPLICATE_TOOL_OUTPUT_PLACEHOLDER.to_string()
                ),
            }
        );
        assert_eq!(
            items[4],
            ResponseItem::FunctionCallOutput {
                call_id: "call-newer".to_string(),
                output: FunctionCallOutputPayload::from_text("newer output".to_string()),
            }
        );
    }

    #[test]
    fn purges_old_failed_tool_input_and_output() {
        let mut failed_output =
            FunctionCallOutputPayload::from_text("huge failure output".to_string());
        failed_output.success = Some(false);
        let mut items = vec![
            user_text("old"),
            ResponseItem::FunctionCall {
                id: None,
                name: "exec_command".to_string(),
                namespace: None,
                arguments: "{\"cmd\":\"bad\"}".to_string(),
                call_id: "call-failed".to_string(),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "call-failed".to_string(),
                output: failed_output,
            },
            user_text("recent"),
        ];

        prune_items_for_prompt(&mut items, config(1, 1));

        assert_eq!(
            items[1],
            ResponseItem::FunctionCall {
                id: None,
                name: "exec_command".to_string(),
                namespace: None,
                arguments: ERROR_TOOL_INPUT_PLACEHOLDER.to_string(),
                call_id: "call-failed".to_string(),
            }
        );
        assert_eq!(
            items[2],
            ResponseItem::FunctionCallOutput {
                call_id: "call-failed".to_string(),
                output: FunctionCallOutputPayload::from_text(
                    ERROR_TOOL_OUTPUT_PLACEHOLDER.to_string()
                ),
            }
        );
    }

    #[test]
    fn checkpoint_scope_older_heavy_prunes_old_large_tool_payloads() {
        let mut output = FunctionCallOutputPayload::from_text(
            "Chunk ID: abc\nProcess exited with code 0\nOutput:\nlarge output body".to_string(),
        );
        output.success = Some(true);
        let mut items = vec![
            user_text("old"),
            ResponseItem::CustomToolCall {
                id: None,
                status: Some("completed".to_string()),
                call_id: "patch-old".to_string(),
                name: "apply_patch".to_string(),
                input:
                    "*** Begin Patch\n*** Add File: huge.md\n+very long patch body\n*** End Patch"
                        .to_string(),
            },
            ResponseItem::CustomToolCallOutput {
                call_id: "patch-old".to_string(),
                name: Some("apply_patch".to_string()),
                output,
            },
            assistant_text("[codelink-context-checkpoint scope=older-heavy] pages 1-8 QA OK"),
            user_text("recent"),
            ResponseItem::FunctionCall {
                id: None,
                name: "exec_command".to_string(),
                namespace: None,
                arguments: "{\"cmd\":\"recent large command that must stay\"}".to_string(),
                call_id: "recent-call".to_string(),
            },
        ];

        prune_items_for_prompt(&mut items, config_with_heavy(1, 10, 20));

        assert!(matches!(
            &items[1],
            ResponseItem::CustomToolCall { input, .. }
                if input.starts_with(HEAVY_TOOL_INPUT_PREFIX)
                    && input.contains("tool=apply_patch")
                    && input.contains("call_id=patch-old")
        ));
        assert!(matches!(
            &items[2],
            ResponseItem::CustomToolCallOutput { output, .. }
                if output.text_content().is_some_and(|text|
                    text.starts_with(HEAVY_TOOL_OUTPUT_PREFIX)
                        && text.contains("tool=apply_patch")
                        && text.contains("call_id=patch-old")
                        && text.contains("success=true")
                        && text.contains("exit_code=0")
                )
        ));
        assert_eq!(items[3], assistant_text(CHECKPOINT_APPLIED_PLACEHOLDER));
        assert_eq!(
            items[5],
            ResponseItem::FunctionCall {
                id: None,
                name: "exec_command".to_string(),
                namespace: None,
                arguments: "{\"cmd\":\"recent large command that must stay\"}".to_string(),
                call_id: "recent-call".to_string(),
            }
        );
    }

    #[test]
    fn checkpoint_scope_older_images_does_not_prune_heavy_tool_text_without_segment_cutoff() {
        let mut items = vec![
            user_text("old"),
            ResponseItem::FunctionCallOutput {
                call_id: "large-output".to_string(),
                output: FunctionCallOutputPayload::from_text(
                    "large output that should stay for image-only checkpoint".to_string(),
                ),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "page1".to_string(),
                output: FunctionCallOutputPayload::from_content_items(vec![
                    FunctionCallOutputContentItem::InputImage {
                        image_url: "data:image/png;base64,page1".to_string(),
                        detail: None,
                    },
                ]),
            },
            assistant_text("[codelink-context-checkpoint scope=older-images] images only"),
            user_text("recent"),
        ];

        prune_items_for_prompt(&mut items, config_with_heavy(1, 10, 20));

        assert_eq!(
            items[1],
            ResponseItem::FunctionCallOutput {
                call_id: "large-output".to_string(),
                output: FunctionCallOutputPayload::from_text(
                    "large output that should stay for image-only checkpoint".to_string()
                ),
            }
        );
        assert!(matches!(
            &items[2],
            ResponseItem::FunctionCallOutput { output, .. }
                if output.content_items().is_some_and(|content_items|
                    matches!(&content_items[0], FunctionCallOutputContentItem::InputText { text }
                        if text.contains(IMAGE_PLACEHOLDER)
                    )
                )
        ));
    }

    #[test]
    fn checkpoint_directive_prunes_requested_turns_without_waiting_for_segment() {
        let mut items = vec![
            user_image("data:image/png;base64,turn1"),
            user_image("data:image/png;base64,turn2"),
            user_image("data:image/png;base64,turn3"),
            assistant_text("[codelink-context-checkpoint turns=3] pages 1-3 QA OK"),
            user_image("data:image/png;base64,turn4"),
        ];

        prune_items_for_prompt(&mut items, config(1, 10));

        let expected_placeholder =
            format!("{IMAGE_PLACEHOLDER}; checkpoint summary: pages 1-3 QA OK");
        assert_eq!(
            items,
            vec![
                ResponseItem::Message {
                    id: None,
                    role: "user".to_string(),
                    content: vec![ContentItem::InputText {
                        text: expected_placeholder.clone(),
                    }],
                    phase: None,
                },
                ResponseItem::Message {
                    id: None,
                    role: "user".to_string(),
                    content: vec![ContentItem::InputText {
                        text: expected_placeholder.clone(),
                    }],
                    phase: None,
                },
                ResponseItem::Message {
                    id: None,
                    role: "user".to_string(),
                    content: vec![ContentItem::InputText {
                        text: expected_placeholder,
                    }],
                    phase: None,
                },
                assistant_text(CHECKPOINT_APPLIED_PLACEHOLDER),
                user_image("data:image/png;base64,turn4"),
            ]
        );
    }

    #[test]
    fn checkpoint_scope_older_images_prunes_seen_image_batch_without_turn_count() {
        let mut items = vec![
            user_text("boot"),
            user_text("task"),
            user_text("make pages"),
            ResponseItem::FunctionCallOutput {
                call_id: "contact-sheet".to_string(),
                output: FunctionCallOutputPayload::from_content_items(vec![
                    FunctionCallOutputContentItem::InputText {
                        text: "contact sheet".to_string(),
                    },
                    FunctionCallOutputContentItem::InputImage {
                        image_url: "data:image/png;base64,sheet".to_string(),
                        detail: None,
                    },
                ]),
            },
            ResponseItem::FunctionCallOutput {
                call_id: "page1".to_string(),
                output: FunctionCallOutputPayload::from_content_items(vec![
                    FunctionCallOutputContentItem::InputImage {
                        image_url: "data:image/png;base64,page1".to_string(),
                        detail: None,
                    },
                ]),
            },
            assistant_text("[codelink-context-checkpoint scope=older-images] pages 1-4 QA OK"),
            user_text("continue"),
        ];

        prune_items_for_prompt(&mut items, config(1, 10));

        let expected_placeholder =
            format!("{IMAGE_PLACEHOLDER}; checkpoint summary: pages 1-4 QA OK");
        assert_eq!(
            items[3],
            ResponseItem::FunctionCallOutput {
                call_id: "contact-sheet".to_string(),
                output: FunctionCallOutputPayload::from_content_items(vec![
                    FunctionCallOutputContentItem::InputText {
                        text: "contact sheet".to_string(),
                    },
                    FunctionCallOutputContentItem::InputText {
                        text: expected_placeholder.clone(),
                    },
                ]),
            }
        );
        assert_eq!(
            items[4],
            ResponseItem::FunctionCallOutput {
                call_id: "page1".to_string(),
                output: FunctionCallOutputPayload::from_content_items(vec![
                    FunctionCallOutputContentItem::InputText {
                        text: expected_placeholder,
                    },
                ]),
            }
        );
        assert_eq!(items[5], assistant_text(CHECKPOINT_APPLIED_PLACEHOLDER));
        assert_eq!(items[6], user_text("continue"));
    }

    #[test]
    fn checkpoint_directive_cannot_prune_recent_guarded_turns() {
        let mut items = vec![
            user_image("data:image/png;base64,turn1"),
            user_image("data:image/png;base64,turn2"),
            assistant_text("[codelink-context-checkpoint scope=older-images] too aggressive"),
        ];

        prune_items_for_prompt(&mut items, config(1, 10));

        assert!(matches!(
            &items[0],
            ResponseItem::Message { content, .. }
                if matches!(&content[0], ContentItem::InputText { .. })
        ));
        assert_eq!(items[1], user_image("data:image/png;base64,turn2"));
        assert_eq!(items[2], assistant_text(CHECKPOINT_APPLIED_PLACEHOLDER));
    }
}
