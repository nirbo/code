//! Message format translation between Codex's internal format and Anthropic's API.

use crate::types::*;
use crate::Result;
use code_protocol::models::ContentItem as ProtocolContentItem;
use code_protocol::models::ResponseItem as ProtocolResponseItem;
use serde_json::json;
use serde_json::Value;

/// Convert Codex protocol messages to Anthropic Messages API format.
pub fn to_anthropic_messages(
    items: &[ProtocolResponseItem],
    system_prompt: &str,
) -> Result<(Vec<Message>, String)> {
    let mut messages = Vec::new();
    let mut current_user_content: Vec<ContentBlock> = Vec::new();
    let mut current_assistant_content: Vec<ContentBlock> = Vec::new();
    let mut _in_user_turn = true;

    // Extract system messages from protocol items (developer/system role messages)
    let system_message = extract_system_prompt(items, system_prompt);

    for item in items {
        match item {
            ProtocolResponseItem::Message { role, content, .. } => {
                match role.as_str() {
                    "user" => {
                        // Flush any pending assistant content
                        if !current_assistant_content.is_empty() {
                            messages.push(Message {
                                role: Role::Assistant,
                                content: if current_assistant_content.len() == 1 {
                                    match &current_assistant_content[0] {
                                        ContentBlock::Text { text } => {
                                            MessageContent::Single(text.clone())
                                        }
                                        _ => MessageContent::Multiple(
                                            std::mem::take(&mut current_assistant_content),
                                        ),
                                    }
                                } else {
                                    MessageContent::Multiple(std::mem::take(
                                        &mut current_assistant_content,
                                    ))
                                },
                            });
                        }

                        // Add user content
                        for block in content {
                            match block {
                                ProtocolContentItem::InputText { text } => {
                                    current_user_content.push(ContentBlock::Text {
                                        text: text.clone(),
                                    });
                                }
                                ProtocolContentItem::InputImage { image_url } => {
                                    if let Some(img_block) = parse_data_url(image_url) {
                                        current_user_content.push(img_block);
                                    }
                                }
                                ProtocolContentItem::OutputText { .. } => {
                                    // Skip output text in user context
                                }
                            }
                        }
                        _in_user_turn = true;
                    }
                    "assistant" => {
                        // Flush any pending user content
                        if !current_user_content.is_empty() {
                            messages.push(Message {
                                role: Role::User,
                                content: if current_user_content.len() == 1 {
                                    match &current_user_content[0] {
                                        ContentBlock::Text { text } => {
                                            MessageContent::Single(text.clone())
                                        }
                                        _ => MessageContent::Multiple(
                                            std::mem::take(&mut current_user_content),
                                        ),
                                    }
                                } else {
                                    MessageContent::Multiple(std::mem::take(&mut current_user_content))
                                },
                            });
                        }

                        // Add assistant content
                        for block in content {
                            match block {
                                ProtocolContentItem::OutputText { text } => {
                                    current_assistant_content.push(ContentBlock::Text {
                                        text: text.clone(),
                                    });
                                }
                                _ => {}
                            }
                        }
                        _in_user_turn = false;
                    }
                    _ => {
                        // Skip other roles (developer, system - these go into system prompt)
                    }
                }
            }
            ProtocolResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            } => {
                // Ensure we have flushed user content first
                if !current_user_content.is_empty() {
                    messages.push(Message {
                        role: Role::User,
                        content: MessageContent::Multiple(std::mem::take(&mut current_user_content)),
                    });
                }

                // Parse the arguments JSON
                let input = serde_json::from_str(arguments).unwrap_or_else(|_| {
                    json!({ "arguments": arguments })
                });

                current_assistant_content.push(ContentBlock::ToolUse {
                    id: call_id.clone(),
                    name: name.clone(),
                    input,
                });
                _in_user_turn = false;
            }
            ProtocolResponseItem::FunctionCallOutput { call_id, output } => {
                // Ensure assistant content is flushed
                if !current_assistant_content.is_empty() {
                    messages.push(Message {
                        role: Role::Assistant,
                        content: MessageContent::Multiple(std::mem::take(
                            &mut current_assistant_content,
                        )),
                    });
                }

                current_user_content.push(ContentBlock::ToolResult {
                    tool_use_id: call_id.clone(),
                    content: vec![ContentBlock::Text {
                        text: output.content.clone(),
                    }],
                    is_error: output
                        .success
                        .map(|s| !s)
                        .or_else(|| Some(false)),
                });
                _in_user_turn = true;
            }
            ProtocolResponseItem::Reasoning { .. } => {
                // Extended thinking will be handled via API parameter
                // Skip reasoning items in message conversion
            }
            _ => {
                // Skip other item types for now
            }
        }
    }

    // Flush remaining content
    if !current_user_content.is_empty() {
        messages.push(Message {
            role: Role::User,
            content: MessageContent::Multiple(current_user_content),
        });
    }
    if !current_assistant_content.is_empty() {
        messages.push(Message {
            role: Role::Assistant,
            content: MessageContent::Multiple(current_assistant_content),
        });
    }

    Ok((messages, system_message))
}

/// Extract the system prompt from protocol items, falling back to the provided prompt.
fn extract_system_prompt(items: &[ProtocolResponseItem], fallback: &str) -> String {
    // Look for developer/system role messages
    for item in items {
        if let ProtocolResponseItem::Message { role, content, .. } = item {
            if role == "developer" || role == "system" {
                let mut text = String::new();
                for block in content {
                    if let ProtocolContentItem::InputText { text: t } = block {
                        text.push_str(t);
                    }
                }
                if !text.is_empty() {
                    return text;
                }
            }
        }
    }
    fallback.to_string()
}

/// Parse a data URL (data:image/png;base64,...) into an ImageSource.
fn parse_data_url(url: &str) -> Option<ContentBlock> {
    if !url.starts_with("data:") {
        return None;
    }

    let parts = url.splitn(3, ':').collect::<Vec<_>>();
    if parts.len() < 2 {
        return None;
    }

    let after_scheme = parts[1];
    let mut parts2 = after_scheme.splitn(2, ';');
    let mime_type = parts2.next()?;
    let encoding = parts2.next()?;

    if !encoding.starts_with("base64,") {
        return None;
    }

    let data = encoding[7..].to_string();

    let media_type = MediaType::from_mime(mime_type)?;

    Some(ContentBlock::Image {
        source: ImageSource {
            media_type,
            data,
        },
    })
}

/// Convert OpenAI tool format to Anthropic tool format.
pub fn to_anthropic_tools(openai_tools: &[Value]) -> Result<Vec<Tool>> {
    let mut anthropic_tools = Vec::new();

    for tool in openai_tools {
        // OpenAI format: { "type": "function", "function": { "name": "...", "description": "...", "parameters": {...} } }
        let function = tool
            .get("function")
            .ok_or_else(|| crate::AnthropicError::ToolTranslation(
                "Missing 'function' key in tool definition".to_string(),
            ))?;

        let name = function
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::AnthropicError::ToolTranslation(
                "Missing 'name' in function definition".to_string(),
            ))?
            .to_string();

        let description = function
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let input_schema = function
            .get("parameters")
            .cloned()
            .unwrap_or_else(|| json!({ "type": "object", "properties": {} }));

        anthropic_tools.push(Tool {
            name,
            description,
            input_schema,
        });
    }

    Ok(anthropic_tools)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_data_url_png() {
        let url = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let result = parse_data_url(url);
        assert!(result.is_some());
        match result.unwrap() {
            ContentBlock::Image { source } => {
                assert_eq!(source.media_type, MediaType::ImagePng);
                assert_eq!(source.data, "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==");
            }
            _ => panic!("Expected Image content block"),
        }
    }

    #[test]
    fn test_parse_data_url_jpeg() {
        let url = "data:image/jpeg;base64,/9j/4AAQSkZJRgABAQEAYABgAAD";
        let result = parse_data_url(url);
        assert!(result.is_some());
        match result.unwrap() {
            ContentBlock::Image { source } => {
                assert_eq!(source.media_type, MediaType::ImageJpeg);
            }
            _ => panic!("Expected Image content block"),
        }
    }

    #[test]
    fn test_anthropic_model_display_names() {
        assert_eq!(AnthropicModel::Opus.display_name(), "Opus");
        assert_eq!(AnthropicModel::Sonnet.display_name(), "Sonnet");
        assert_eq!(AnthropicModel::Haiku.display_name(), "Haiku");
    }
}
