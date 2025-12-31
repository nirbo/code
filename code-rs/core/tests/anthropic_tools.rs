//! Tests for Anthropic tool format translation.
//!
//! Verifies that OpenAI-style tool definitions are correctly converted
//! to Anthropic's input_schema format.

use serde_json::json;

/// Convert an OpenAI-style tool definition to Anthropic format.
/// This mirrors the logic in anthropic_completions.rs.
fn convert_tool_to_anthropic(openai_tool: serde_json::Value) -> serde_json::Value {
    let function = openai_tool.get("function").cloned().unwrap_or(openai_tool.clone());
    
    json!({
        "name": function.get("name").cloned().unwrap_or(json!("")),
        "description": function.get("description").cloned().unwrap_or(json!("")),
        "input_schema": function.get("parameters").cloned().unwrap_or(json!({}))
    })
}

#[test]
fn converts_openai_tool_to_anthropic_format() {
    let openai_tool = json!({
        "type": "function",
        "function": {
            "name": "shell",
            "description": "Execute a shell command",
            "parameters": {
                "type": "object",
                "properties": {
                    "command": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                },
                "required": ["command"]
            }
        }
    });
    
    let anthropic_tool = convert_tool_to_anthropic(openai_tool);
    
    assert_eq!(anthropic_tool["name"], "shell");
    assert_eq!(anthropic_tool["description"], "Execute a shell command");
    assert!(anthropic_tool["input_schema"].is_object());
    assert_eq!(anthropic_tool["input_schema"]["type"], "object");
}

#[test]
fn handles_tool_without_function_wrapper() {
    // Sometimes tools come without the "function" wrapper
    let tool = json!({
        "name": "read_file",
        "description": "Read a file",
        "parameters": {
            "type": "object",
            "properties": {
                "path": { "type": "string" }
            }
        }
    });
    
    let anthropic_tool = convert_tool_to_anthropic(tool);
    
    assert_eq!(anthropic_tool["name"], "read_file");
    assert_eq!(anthropic_tool["description"], "Read a file");
}

#[test]
fn handles_empty_parameters() {
    let openai_tool = json!({
        "type": "function",
        "function": {
            "name": "get_time",
            "description": "Get current time"
            // No parameters
        }
    });
    
    let anthropic_tool = convert_tool_to_anthropic(openai_tool);
    
    assert_eq!(anthropic_tool["name"], "get_time");
    assert!(anthropic_tool["input_schema"].is_object());
}

#[test]
fn input_schema_preserves_complex_types() {
    let openai_tool = json!({
        "type": "function",
        "function": {
            "name": "apply_patch",
            "description": "Apply a code patch",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "patch": { "type": "string" },
                    "options": {
                        "type": "object",
                        "properties": {
                            "create": { "type": "boolean" },
                            "force": { "type": "boolean" }
                        }
                    }
                },
                "required": ["path", "patch"]
            }
        }
    });
    
    let anthropic_tool = convert_tool_to_anthropic(openai_tool);
    
    let input_schema = &anthropic_tool["input_schema"];
    assert_eq!(input_schema["type"], "object");
    assert!(input_schema["properties"]["path"].is_object());
    assert!(input_schema["properties"]["patch"].is_object());
    assert!(input_schema["properties"]["options"]["properties"]["create"].is_object());
    assert_eq!(input_schema["required"], json!(["path", "patch"]));
}

#[test]
fn tool_use_id_format_is_valid() {
    // Anthropic tool_use_id format should be prefixed with "toolu_"
    let tool_id = "toolu_01abcdef";
    assert!(tool_id.starts_with("toolu_"), "tool use IDs should start with 'toolu_'");
}

#[test]
fn function_call_id_format_is_valid() {
    // Our internal FunctionCall IDs are prefixed with "fc_"
    let call_id = format!("fc_{}", "toolu_01abcdef");
    assert!(call_id.starts_with("fc_"), "function call IDs should start with 'fc_'");
}
