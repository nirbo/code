//! Tool format translation between OpenAI and Anthropic formats.

use crate::Result;
use serde_json::json;
use serde_json::Value;

/// Convert OpenAI tool format to Anthropic tool format.
///
/// OpenAI format:
/// ```json
/// {
///   "type": "function",
///   "function": {
///     "name": "tool_name",
///     "description": "Tool description",
///     "parameters": { ... JSON Schema ... }
///   }
/// }
/// ```
///
/// Anthropic format:
/// ```json
/// {
///   "name": "tool_name",
///   "description": "Tool description",
///   "input_schema": { ... JSON Schema ... }
/// }
/// ```
pub fn openai_to_anthropic_tools(openai_tools: &[Value]) -> Result<Vec<Value>> {
    let mut anthropic_tools = Vec::new();

    for tool in openai_tools {
        let function = tool
            .get("function")
            .ok_or_else(|| crate::AnthropicError::ToolTranslation(
                "Tool missing 'function' key".to_string(),
            ))?;

        let name = function
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| crate::AnthropicError::ToolTranslation(
                "Tool function missing 'name'".to_string(),
            ))?
            .to_string();

        let description = function
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // Get parameters, defaulting to empty object schema
        let parameters = function.get("parameters").cloned().unwrap_or_else(|| {
            json!({
                "type": "object",
                "properties": {}
            })
        });

        // Ensure the parameters schema has the required structure
        let input_schema = ensure_json_schema_structure(&parameters)?;

        anthropic_tools.push(json!({
            "name": name,
            "description": description,
            "input_schema": input_schema
        }));
    }

    Ok(anthropic_tools)
}

/// Ensure the JSON Schema has the structure required by Anthropic.
///
/// Anthropic requires:
/// - type: "object" (or other valid types)
/// - Optional: properties (for object type)
/// - Optional: required (array of strings)
fn ensure_json_schema_structure(schema: &Value) -> Result<Value> {
    let mut result = schema.clone();

    // If no type is specified, default to object
    if result.get("type").is_none() {
        if let Some(obj) = result.as_object_mut() {
            obj.insert("type".to_string(), json!("object"));
        }
    }

    // Ensure properties exist for object types
    if result.get("type").and_then(|v| v.as_str()) == Some("object") {
        if result.get("properties").is_none() {
            if let Some(obj) = result.as_object_mut() {
                obj.insert("properties".to_string(), json!({}));
            }
        }
    }

    Ok(result)
}

/// Convert Anthropic tool use block to OpenAI format.
///
/// This is used when converting streaming responses back to the internal format.
pub fn anthropic_to_openai_tool_use(
    id: &str,
    name: &str,
    input: &Value,
) -> Value {
    json!({
        "id": id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": input.to_string()
        }
    })
}

/// Convert Anthropic tool result to OpenAI format.
pub fn anthropic_to_openai_tool_result(
    tool_use_id: &str,
    content: &str,
    _is_error: Option<bool>,
) -> Value {
    json!({
        "role": "tool",
        "tool_call_id": tool_use_id,
        "content": content
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_to_anthropic_tools_basic() {
        let openai_tools = vec![json!({
            "type": "function",
            "function": {
                "name": "test_tool",
                "description": "A test tool",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "arg1": { "type": "string" }
                    }
                }
            }
        })];

        let result = openai_to_anthropic_tools(&openai_tools).unwrap();
        assert_eq!(result.len(), 1);

        let tool = &result[0];
        assert_eq!(tool["name"], "test_tool");
        assert_eq!(tool["description"], "A test tool");
        assert_eq!(tool["input_schema"]["type"], "object");
        assert!(tool["input_schema"]["properties"].is_object());
    }

    #[test]
    fn test_openai_to_anthropic_tools_no_parameters() {
        let openai_tools = vec![json!({
            "type": "function",
            "function": {
                "name": "simple_tool",
                "description": "A simple tool"
            }
        })];

        let result = openai_to_anthropic_tools(&openai_tools).unwrap();
        assert_eq!(result.len(), 1);

        let tool = &result[0];
        assert_eq!(tool["name"], "simple_tool");
        assert_eq!(tool["input_schema"]["type"], "object");
        assert!(tool["input_schema"]["properties"].as_object().map_or(true, |p| p.is_empty()));
    }

    #[test]
    fn test_anthropic_to_openai_tool_use() {
        let result = anthropic_to_openai_tool_use(
            "tool_123",
            "test_function",
            &json!({"arg1": "value1"}),
        );

        assert_eq!(result["id"], "tool_123");
        assert_eq!(result["type"], "function");
        assert_eq!(result["function"]["name"], "test_function");
        assert_eq!(result["function"]["arguments"], r#"{"arg1":"value1"}"#);
    }

    #[test]
    fn test_ensure_json_schema_structure() {
        let schema = json!({"properties": {"arg1": {"type": "string"}}});
        let result = ensure_json_schema_structure(&schema).unwrap();

        assert_eq!(result["type"], "object");
        assert!(result["properties"].is_object());
    }

    #[test]
    fn test_ensure_json_schema_structure_preserves_existing() {
        let schema = json!({
            "type": "object",
            "properties": {"arg1": {"type": "string"}},
            "required": ["arg1"]
        });
        let result = ensure_json_schema_structure(&schema).unwrap();

        assert_eq!(result["type"], "object");
        assert_eq!(result["required"], json!(["arg1"]));
    }
}
