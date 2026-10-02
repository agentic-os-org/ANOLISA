use serde_json::json;

#[test]
fn static_tool_hint_respects_description_budget_and_restores_unicode() {
    use tokenless_ccr::{InMemoryStore, RecoveryMethod, StashStore, recovery_hashes};
    let store = std::sync::Arc::new(InMemoryStore::new());
    let method = RecoveryMethod::tool("t".repeat(64)).unwrap();
    let description = "详细说明。".repeat(300);
    let schema = json!({"function": {"name": "lookup", "description": description}});
    let output = SchemaCompressor::new()
        .with_stash_store(store.clone())
        .with_recovery(method.clone())
        .compress(&schema);
    let text = output["function"]["description"].as_str().unwrap();
    assert!(text.chars().count() <= 256);
    assert!(!text.contains("<<tokenless:"));
    let hashes = recovery_hashes(text, &method);
    assert_eq!(hashes.len(), 1);
    assert_eq!(store.retrieve(hashes[0]).unwrap().unwrap(), description);
}

#[test]
fn test_compress_long_description() {
    let compressor = SchemaCompressor::new();
    let schema = json!({
        "function": {
            "name": "test_func",
            "description": "This is a very long description that should be truncated. It contains a lot of text that goes on and on. The quick brown fox jumps over the lazy dog. Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris.",
            "parameters": {
                "type": "object",
                "properties": {
                    "param1": {
                        "type": "string",
                        "description": "Another long description for a parameter that should be truncated to a shorter length. This text is intentionally verbose to test the truncation logic properly."
                    }
                }
            }
        }
    });

    let result = compressor.compress(&schema);

    // Function description should be truncated to <= 256
    let func_desc = result["function"]["description"].as_str().unwrap();
    assert!(func_desc.len() <= 256);

    // Parameter description should be truncated to <= 160
    let param_desc = result["function"]["parameters"]["properties"]["param1"]["description"]
        .as_str()
        .unwrap();
    assert!(param_desc.len() <= 160);
}

#[test]
fn compress_openai_tools_request_container() {
    let compressor = SchemaCompressor::new();
    let schema = json!({
        "model": "example-model",
        "tool_choice": "auto",
        "tools": [
            {
                "type": "function",
                "function": {
                    "name": "lookup",
                    "description": "A".repeat(2000),
                    "parameters": {
                        "type": "object",
                        "properties": {
                            "query": {
                                "type": "string",
                                "description": "B".repeat(1000)
                            }
                        }
                    }
                }
            },
            {
                "type": "web_search_preview",
                "search_context_size": "low",
                "title": "Preserve built-in tool metadata",
                "description": "C".repeat(400),
                "examples": ["preserve"]
            }
        ]
    });

    let result = compressor.compress(&schema);
    let function = &result["tools"][0]["function"];

    assert!(function["description"].as_str().unwrap().chars().count() <= 256);
    assert!(
        function["parameters"]["properties"]["query"]["description"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 160
    );
    assert_eq!(result["model"], "example-model");
    assert_eq!(result["tool_choice"], "auto");
    assert_eq!(result["tools"][1], schema["tools"][1]);
}

#[test]
fn compress_gemini_tools_request_container() {
    let compressor = SchemaCompressor::new();
    let schema = json!({
        "model": "example-model",
        "tools": [{
            "functionDeclarations": [{
                "name": "lookup",
                "description": "A".repeat(2000),
                "parametersJsonSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "B".repeat(1000)
                        }
                    }
                }
            }]
        }]
    });

    let result = compressor.compress(&schema);
    let function = &result["tools"][0]["functionDeclarations"][0];

    assert!(function["description"].as_str().unwrap().chars().count() <= 256);
    assert!(
        function["parametersJsonSchema"]["properties"]["query"]["description"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 160
    );
    assert_eq!(result["model"], schema["model"]);
}

#[test]
fn compress_bare_declaration_in_tools_request_container() {
    let compressor = SchemaCompressor::new();
    let schema = json!({
        "model": "example-model",
        "tools": [{
            "name": "lookup",
            "description": "A".repeat(2000),
            "parameters": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "B".repeat(1000)
                    }
                }
            }
        }]
    });

    let result = compressor.compress(&schema);
    let function = &result["tools"][0];

    assert!(function["description"].as_str().unwrap().chars().count() <= 256);
    assert!(
        function["parameters"]["properties"]["query"]["description"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 160
    );
    assert_eq!(result["model"], schema["model"]);
}

#[test]
fn test_protected_fields_preserved() {
    let compressor = SchemaCompressor::new();
    let schema = json!({
        "function": {
            "name": "my_function",
            "parameters": {
                "type": "object",
                "required": ["field1"],
                "properties": {
                    "field1": {
                        "type": "string",
                        "enum": ["a", "b", "c"],
                        "default": "a",
                        "const": "fixed_value"
                    }
                }
            }
        }
    });

    let result = compressor.compress(&schema);

    // Verify protected fields are preserved
    assert_eq!(result["function"]["name"], "my_function");
    assert_eq!(result["function"]["parameters"]["type"], "object");
    assert!(result["function"]["parameters"]["required"].is_array());
    assert!(result["function"]["parameters"]["properties"]["field1"]["enum"].is_array());
    assert_eq!(
        result["function"]["parameters"]["properties"]["field1"]["default"],
        "a"
    );
    assert_eq!(
        result["function"]["parameters"]["properties"]["field1"]["const"],
        "fixed_value"
    );
}

#[test]
fn test_compress_gemini_function_declarations() {
    let compressor = SchemaCompressor::new();
    let schema = json!({
        "functionDeclarations": [
            {
                "name": "task",
                "description": "Launch a new agent to handle complex, multi-step tasks autonomously. It contains a lot of text that goes on and on. The quick brown fox jumps over the lazy dog. Lorem ipsum dolor sit amet, consectetur adipiscing elit. Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "subagent_type": {
                            "type": "string",
                            "description": "Another long description for a parameter that should be truncated to a shorter length. This text is intentionally verbose to test the truncation logic properly.",
                            "enum": ["general-purpose", "code-reviewer"]
                        }
                    },
                    "required": ["subagent_type"]
                }
            }
        ]
    });

    let result = compressor.compress(&schema);

    // Wrapper structure preserved
    assert!(result.get("functionDeclarations").is_some());
    let decls = result["functionDeclarations"].as_array().unwrap();
    assert_eq!(decls.len(), 1);

    // Name, type, required, enum preserved
    assert_eq!(decls[0]["name"], "task");
    assert_eq!(decls[0]["parameters"]["type"], "object");
    assert!(decls[0]["parameters"]["required"].is_array());
    assert_eq!(
        decls[0]["parameters"]["properties"]["subagent_type"]["enum"],
        json!(["general-purpose", "code-reviewer"])
    );

    // Function description truncated to <= 256
    let func_desc = decls[0]["description"].as_str().unwrap();
    assert!(func_desc.len() <= 256);
    assert!(
        func_desc.len()
            < schema["functionDeclarations"][0]["description"]
                .as_str()
                .unwrap()
                .len()
    );

    // Parameter description truncated to <= 160
    let param_desc = decls[0]["parameters"]["properties"]["subagent_type"]["description"]
        .as_str()
        .unwrap();
    assert!(param_desc.len() <= 160);
}

#[test]
fn test_gemini_tool_preserves_non_schema_keys() {
    let compressor = SchemaCompressor::new();
    let schema = json!({
        "functionDeclarations": [
            {
                "name": "greet",
                "description": "Say hello",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Name to greet" }
                    }
                }
            }
        ],
        "googleSearchRetrieval": {}
    });

    let result = compressor.compress(&schema);

    // Non-schema Tool key untouched
    assert!(result.get("googleSearchRetrieval").is_some());
    // functionDeclarations still present
    assert_eq!(result["functionDeclarations"][0]["name"], "greet");
}

#[test]
fn test_gemini_empty_function_declarations_no_panic() {
    let compressor = SchemaCompressor::new();
    let result = compressor.compress(&json!({"functionDeclarations": []}));
    assert!(result["functionDeclarations"].is_array());
    assert!(
        result["functionDeclarations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn test_compress_gemini_parameters_json_schema() {
    // Mirrors copilot-shell's DeclarativeTool.schema payload: the parameter
    // schema lives under `parametersJsonSchema` (Gemini SDK JSON Schema
    // format), not `parameters`. Without explicit handling, parameter-level
    // descriptions/titles/examples would escape compression entirely.
    let compressor = SchemaCompressor::new();
    let schema = json!({
        "functionDeclarations": [{
            "name": "write_file",
            "description": "Write a file to the local filesystem. This is a deliberately long description that exceeds the default 256-character function description limit so the compressor must truncate it. The quick brown fox jumps over the lazy dog. Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam.",
            "parametersJsonSchema": {
                "title": "WriteFileParams",
                "type": "object",
                "properties": {
                    "path": {
                        "title": "Path",
                        "type": "string",
                        "description": "Absolute path of the file to write. This parameter description is intentionally verbose to exceed the default 160-character parameter description limit and verify truncation applies to the parametersJsonSchema branch, not just the legacy parameters field.",
                        "examples": ["/tmp/example.txt"]
                    }
                },
                "required": ["path"],
                "additionalProperties": false,
                "$schema": "http://json-schema.org/draft-07/schema#"
            }
        }]
    });

    let result = compressor.compress(&schema);
    let decl = &result["functionDeclarations"][0];

    // The parametersJsonSchema field is preserved (not renamed to parameters).
    assert!(decl.get("parametersJsonSchema").is_some());
    assert!(decl.get("parameters").is_none());
    let params = &decl["parametersJsonSchema"];

    // Structural keys preserved.
    assert_eq!(params["type"], "object");
    assert_eq!(params["required"], json!(["path"]));
    assert_eq!(params["additionalProperties"], false);
    assert_eq!(params["$schema"], "http://json-schema.org/draft-07/schema#");

    // Function description truncated to <= 256.
    let func_desc = decl["description"].as_str().unwrap();
    assert!(func_desc.chars().count() <= 256);

    // Parameter description truncated to <= 160.
    let param_desc = params["properties"]["path"]["description"]
        .as_str()
        .unwrap();
    assert!(param_desc.chars().count() <= 160);

    // Title and examples dropped (drop_titles / drop_examples default true).
    assert!(params.get("title").is_none());
    assert!(params["properties"]["path"].get("title").is_none());
    assert!(params["properties"]["path"].get("examples").is_none());
}

#[test]
fn test_parameters_json_schema_stash_roundtrip() {
    // Regression: copilot-shell's DeclarativeTool.schema puts the parameter
    // schema under `parametersJsonSchema`. With a stash store attached,
    // truncated descriptions must carry a retrievable marker and retrieve
    // must yield the verbatim original — for both the function-level
    // description and nested parameter descriptions.
    use std::sync::Arc;
    use tokenless_ccr::{InMemoryStore, StashStore, extract_hash};

    let store = Arc::new(InMemoryStore::new());
    let compressor = SchemaCompressor::new()
        .with_func_desc_max_len(100)
        .with_param_desc_max_len(120)
        .with_stash_store(store.clone());

    let func_desc_orig = format!("FUNCORIG_{}", "a".repeat(200));
    let param_desc_orig = format!("PARAMORIG_{}", "b".repeat(200));
    let schema = json!({
        "functionDeclarations": [{
            "name": "write_file",
            "description": func_desc_orig,
            "parametersJsonSchema": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": param_desc_orig
                    }
                },
                "required": ["path"]
            }
        }]
    });

    let result = compressor.compress(&schema);
    let decl = &result["functionDeclarations"][0];

    // Function-level description: marker present, fits limit, retrieves verbatim.
    let func_desc = decl["description"].as_str().unwrap();
    assert!(
        func_desc.contains("If needed, run in shell: tokenless retrieve "),
        "function desc must carry marker"
    );
    assert!(func_desc.chars().count() <= 100);
    let func_key = extract_hash(func_desc).expect("function desc marker has hash");
    let func_retrieved = store.retrieve(func_key).unwrap().unwrap();
    assert_eq!(func_retrieved, func_desc_orig);
    assert!(!func_retrieved.contains("If needed, run in shell: tokenless retrieve "));

    // Parameter-level description: marker present, fits limit, retrieves verbatim.
    let param_desc = decl["parametersJsonSchema"]["properties"]["path"]["description"]
        .as_str()
        .unwrap();
    assert!(
        param_desc.contains("If needed, run in shell: tokenless retrieve "),
        "param desc must carry marker"
    );
    assert!(param_desc.chars().count() <= 120);
    let param_key = extract_hash(param_desc).expect("param desc marker has hash");
    let param_retrieved = store.retrieve(param_key).unwrap().unwrap();
    assert_eq!(param_retrieved, param_desc_orig);
    assert!(!param_retrieved.contains("If needed, run in shell: tokenless retrieve "));
}

#[test]
fn test_title_and_examples_removed() {
    let compressor = SchemaCompressor::new();
    let schema = json!({
        "function": {
            "name": "test",
            "title": "Test Function Title",
            "parameters": {
                "type": "object",
                "title": "Parameters Title",
                "properties": {
                    "field1": {
                        "type": "string",
                        "title": "Field Title",
                        "examples": ["example1", "example2"]
                    }
                }
            }
        }
    });

    let result = compressor.compress(&schema);

    // Titles should be removed
    assert!(result["function"].get("title").is_none());
    assert!(result["function"]["parameters"].get("title").is_none());
    assert!(
        result["function"]["parameters"]["properties"]["field1"]
            .get("title")
            .is_none()
    );

    // Examples should be removed
    assert!(
        result["function"]["parameters"]["properties"]["field1"]
            .get("examples")
            .is_none()
    );
}

#[test]
fn test_empty_schema_no_panic() {
    let compressor = SchemaCompressor::new();

    // Empty object
    let result = compressor.compress(&json!({}));
    assert!(result.is_object());

    // Null
    let result = compressor.compress(&Value::Null);
    assert!(result.is_null());

    // Empty function
    let result = compressor.compress(&json!({"function": {}}));
    assert!(result["function"].is_object());
}

#[test]
fn test_nested_properties_recursive_compression() {
    let compressor = SchemaCompressor::new();
    let schema = json!({
        "function": {
            "name": "nested_test",
            "parameters": {
                "type": "object",
                "properties": {
                    "level1": {
                        "type": "object",
                        "title": "Level 1 Title",
                        "description": "Level 1 description that is quite long and should be truncated according to the parameter max length setting.",
                        "properties": {
                            "level2": {
                                "type": "object",
                                "title": "Level 2 Title",
                                "examples": ["ex1"],
                                "properties": {
                                    "level3": {
                                        "type": "string",
                                        "title": "Level 3 Title"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    });

    let result = compressor.compress(&schema);

    // Check nested titles are removed
    assert!(
        result["function"]["parameters"]["properties"]["level1"]
            .get("title")
            .is_none()
    );
    assert!(
        result["function"]["parameters"]["properties"]["level1"]["properties"]["level2"]
            .get("title")
            .is_none()
    );
    assert!(
        result["function"]["parameters"]["properties"]["level1"]["properties"]["level2"]
            ["properties"]["level3"]
            .get("title")
            .is_none()
    );

    // Check nested examples are removed
    assert!(
        result["function"]["parameters"]["properties"]["level1"]["properties"]["level2"]
            .get("examples")
            .is_none()
    );
}

#[test]
fn test_truncate_at_sentence_boundary() {
    let compressor = SchemaCompressor::new();
    // Sentence boundary at position ~40 which is in range [30, 60]
    let text = "Short intro text for testing. This sentence ends here. More text follows after that point.";

    let result = compressor.truncate_description(text, 60);

    // Should truncate at a sentence boundary
    assert!(
        result.ends_with('.'),
        "Result '{result}' should end with '.'"
    );
    assert!(result.len() <= 60);
}

#[test]
fn test_markdown_removal() {
    let compressor = SchemaCompressor::new();
    let text = "Some text with ```code block``` and `inline code` markers.";

    let result = compressor.truncate_description(text, 256);

    assert!(!result.contains("```"));
    assert!(!result.contains('`'));
}

#[test]
fn test_anyof_oneof_allof_compression() {
    let compressor = SchemaCompressor::new();
    let schema = json!({
        "function": {
            "name": "combo_test",
            "parameters": {
                "type": "object",
                "properties": {
                    "field1": {
                        "anyOf": [
                            {"type": "string", "title": "String Option", "examples": ["ex"]},
                            {"type": "number", "title": "Number Option"}
                        ]
                    },
                    "field2": {
                        "oneOf": [
                            {"type": "boolean", "title": "Bool Option"}
                        ]
                    },
                    "field3": {
                        "allOf": [
                            {"type": "object", "title": "Obj Option"}
                        ]
                    }
                }
            }
        }
    });

    let result = compressor.compress(&schema);

    // Check anyOf items are compressed
    assert!(
        result["function"]["parameters"]["properties"]["field1"]["anyOf"][0]
            .get("title")
            .is_none()
    );
    assert!(
        result["function"]["parameters"]["properties"]["field1"]["anyOf"][0]
            .get("examples")
            .is_none()
    );

    // Check oneOf items are compressed
    assert!(
        result["function"]["parameters"]["properties"]["field2"]["oneOf"][0]
            .get("title")
            .is_none()
    );

    // Check allOf items are compressed
    assert!(
        result["function"]["parameters"]["properties"]["field3"]["allOf"][0]
            .get("title")
            .is_none()
    );
}

#[test]
fn max_depth_stops_recursion() {
    // Build a 100-level schema and verify with_max_depth bounds the
    // recursive descent — descriptions below the limit must be left
    // untouched, descriptions above must be truncated.
    let compressor = SchemaCompressor::new().with_max_depth(5);
    let long_desc = "x".repeat(400);
    let mut schema = json!({
        "type": "string",
        "description": long_desc.clone(),
    });
    for _ in 0..100 {
        schema = json!({
            "type": "object",
            "description": long_desc.clone(),
            "properties": {"nested": schema},
        });
    }
    let result = compressor.compress(&schema);
    // Top-level description (depth 0) must be truncated.
    let top = result["description"].as_str().unwrap();
    assert!(top.chars().count() <= 256);
    // Walk down 10 levels — well past max_depth — and confirm we still
    // see the original 400-char description (recursion stopped early).
    let mut node = &result;
    for _ in 0..10 {
        node = &node["properties"]["nested"];
    }
    let deep = node["description"].as_str().unwrap();
    assert_eq!(deep.chars().count(), 400);
}

#[test]
fn truncate_description_cjk_no_panic() {
    let compressor = SchemaCompressor::new();
    // 100 CJK chars fit within 256-char limit — no truncation needed
    let cjk = "中".repeat(100);
    let result = compressor.truncate_description(&cjk, 256);
    assert!(result.chars().all(|c| c == '中'));
    assert!(result.chars().count() <= 256);

    // 300 CJK chars exceed 256-char limit — should be truncated
    let cjk_long = "中".repeat(300);
    let result_long = compressor.truncate_description(&cjk_long, 256);
    assert!(result_long.chars().count() <= 256);
}

#[test]
fn builder_with_func_desc_max_len() {
    let c = SchemaCompressor::new().with_func_desc_max_len(50);
    let long = "A".repeat(100);
    let schema = json!({
        "function": {
            "name": "test",
            "description": long,
            "parameters": {"type": "object", "properties": {}}
        }
    });
    let result = c.compress(&schema);
    let desc = result["function"]["description"].as_str().unwrap();
    assert!(desc.chars().count() <= 50);
}

#[test]
fn builder_with_param_desc_max_len() {
    let c = SchemaCompressor::new().with_param_desc_max_len(30);
    let long = "B".repeat(100);
    let schema = json!({
        "function": {
            "name": "test",
            "parameters": {
                "type": "object",
                "properties": {
                    "p": {"type": "string", "description": long}
                }
            }
        }
    });
    let result = c.compress(&schema);
    let desc = result["function"]["parameters"]["properties"]["p"]["description"]
        .as_str()
        .unwrap();
    assert!(desc.chars().count() <= 30);
}

#[test]
fn builder_with_drop_examples_false_preserves() {
    let c = SchemaCompressor::new().with_drop_examples(false);
    let schema = json!({
        "function": {
            "name": "test",
            "parameters": {
                "type": "object",
                "properties": {
                    "p": {"type": "string", "examples": ["a", "b"]}
                }
            }
        }
    });
    let result = c.compress(&schema);
    assert!(
        result["function"]["parameters"]["properties"]["p"]
            .get("examples")
            .is_some()
    );
}

#[test]
fn builder_with_drop_titles_false_preserves() {
    let c = SchemaCompressor::new().with_drop_titles(false);
    let schema = json!({
        "function": {
            "name": "test",
            "title": "Keep This",
            "parameters": {
                "type": "object",
                "title": "Params",
                "properties": {}
            }
        }
    });
    let result = c.compress(&schema);
    assert_eq!(result["function"]["title"], "Keep This");
    assert_eq!(result["function"]["parameters"]["title"], "Params");
}

#[test]
fn builder_with_drop_markdown_false_preserves() {
    let c = SchemaCompressor::new().with_drop_markdown(false);
    let text = "Use `code` in description.";
    let result = c.truncate_description(text, 256);
    assert!(result.contains('`'));
}

#[test]
fn compress_direct_schema_no_function_wrapper() {
    let c = SchemaCompressor::new();
    let long = "D".repeat(400);
    let schema = json!({
        "type": "object",
        "title": "TopLevel",
        "description": long,
        "properties": {
            "name": {"type": "string", "title": "FieldTitle"}
        }
    });
    let result = c.compress(&schema);
    assert!(result.get("title").is_none());
    let desc = result["description"].as_str().unwrap();
    assert!(desc.chars().count() <= 256);
    assert!(result["properties"]["name"].get("title").is_none());
}

#[test]
fn char_index_empty_string() {
    assert_eq!(char_index("", 0), 0);
    assert_eq!(char_index("", 5), 0);
}

#[test]
fn char_index_beyond_length() {
    assert_eq!(char_index("abc", 10), 3);
}

#[test]
fn char_index_multibyte() {
    let text = "你好world";
    assert_eq!(char_index(text, 0), 0);
    assert_eq!(char_index(text, 2), 6); // 2 CJK chars = 6 bytes
}

#[test]
fn test_description_truncation_with_stash() {
    use std::sync::Arc;
    use tokenless_ccr::{InMemoryStore, StashStore, extract_hash};

    let store = Arc::new(InMemoryStore::new());
    let compressor = SchemaCompressor::new()
        .with_func_desc_max_len(100)
        .with_stash_store(store.clone());
    let long_desc = "A".repeat(300);
    let schema = json!({
        "function": {
            "name": "test",
            "description": long_desc,
            "parameters": {"type": "object", "properties": {}}
        }
    });
    let result = compressor.compress(&schema);
    let desc = result["function"]["description"].as_str().unwrap();
    assert!(desc.chars().count() <= 100);
    assert!(desc.contains("If needed, run in shell: tokenless retrieve "));
    let hash = extract_hash(desc).unwrap();
    let retrieved = store.retrieve(hash).unwrap().unwrap();
    assert_eq!(retrieved, long_desc);
}

#[test]
fn direct_schema_stash_single_retrieve() {
    // Regression test: direct schema with description > func_desc_max_len must
    // stash exactly once. The retrieved value must be the verbatim original —
    // no nested <<tokenless:K1>> markers.
    use std::sync::Arc;
    use tokenless_ccr::{InMemoryStore, StashStore, extract_hash};

    let store = Arc::new(InMemoryStore::new());
    let compressor = SchemaCompressor::new()
        .with_func_desc_max_len(100)
        .with_stash_store(store.clone());

    let original_desc = format!("DIRECTORIG_{}", "a".repeat(280));
    let schema = json!({
        "type": "object",
        "description": original_desc,
        "properties": {
            "p": {"description": "b".repeat(180)}
        }
    });

    let result = compressor.compress(&schema);
    let desc = result["description"].as_str().unwrap();

    // Output marker must be present and fit within the limit.
    assert!(
        desc.contains("If needed, run in shell: tokenless retrieve "),
        "expected a stash marker in output"
    );
    assert!(desc.chars().count() <= 100);

    // Retrieve the single stash key — must yield the verbatim original with
    // no nested markers.
    let key = extract_hash(desc).expect("marker must carry a valid hash");
    let retrieved = store
        .retrieve(key)
        .unwrap()
        .expect("stash entry must exist");
    assert_eq!(
        retrieved, original_desc,
        "retrieved value must equal the original description verbatim"
    );
    assert!(
        !retrieved.contains("If needed, run in shell: tokenless retrieve "),
        "retrieved value must not contain nested stash markers"
    );
}

#[test]
fn test_compress_parameters_with_nested_schema() {
    let compressor = SchemaCompressor::new();
    let schema = json!({
        "function": {
            "name": "test",
            "parameters": {
                "type": "object",
                "properties": {
                    "config": {
                        "type": "object",
                        "title": "Config Title",
                        "examples": ["ex1"],
                        "properties": {
                            "nested": {
                                "type": "string",
                                "title": "Nested Title",
                                "description": "B".repeat(200)
                            }
                        }
                    }
                }
            }
        }
    });
    let result = compressor.compress(&schema);
    let props = &result["function"]["parameters"]["properties"];
    assert!(props["config"].get("title").is_none());
    assert!(props["config"].get("examples").is_none());
    assert!(
        props["config"]["properties"]["nested"]
            .get("title")
            .is_none()
    );
    let nested_desc = props["config"]["properties"]["nested"]["description"]
        .as_str()
        .unwrap();
    assert!(nested_desc.chars().count() <= 160);
}

#[test]
fn test_rollback_stash_writes_removes_created_entries() {
    use std::sync::Arc;
    use tokenless_ccr::InMemoryStore;

    let store = Arc::new(InMemoryStore::new());
    let compressor = SchemaCompressor::new()
        .with_func_desc_max_len(100)
        .with_stash_store(store.clone());
    let schema = json!({
        "function": {
            "name": "test",
            "description": "A".repeat(300),
            "parameters": {"type": "object", "properties": {}}
        }
    });
    let _ = compressor.compress(&schema);
    assert_eq!(compressor.stash_writes(), 1);
    assert_eq!(store.len(), 1);

    let removed = compressor.rollback_stash_writes();
    assert_eq!(removed, 1);
    assert_eq!(store.len(), 0);
    assert_eq!(compressor.stash_writes(), 0);
    assert_eq!(compressor.rollback_stash_writes(), 0);
}

#[test]
fn test_rollback_preserves_preexisting_same_payload_entry() {
    // Refreshing an already-emitted description must not put that key on the
    // rollback list — discarding a later no-savings compress must not make
    // earlier markers unretrievable.
    use std::sync::Arc;
    use tokenless_ccr::{InMemoryStore, StashStore};

    let store = Arc::new(InMemoryStore::new());
    let long_desc = "A".repeat(300);
    let write = store.stash(&long_desc).unwrap();
    let hash = write.key;
    assert!(write.created);

    let compressor = SchemaCompressor::new()
        .with_func_desc_max_len(100)
        .with_stash_store(store.clone());
    let schema = json!({
        "function": {
            "name": "test",
            "description": long_desc,
            "parameters": {"type": "object", "properties": {}}
        }
    });
    let _ = compressor.compress(&schema);
    assert_eq!(store.len(), 1);
    let removed = compressor.rollback_stash_writes();
    assert_eq!(removed, 0, "refresh must not be treated as created");
    assert_eq!(
        store.retrieve(&hash).unwrap().as_deref(),
        Some(long_desc.as_str()),
        "pre-existing emitted marker must remain retrievable after rollback"
    );
}

#[test]
fn test_batch_rollback_removes_keys_from_every_item() {
    // CLI --batch calls compress() once per item then discards the whole
    // batch on no-savings. Keys must accumulate across compress() calls.
    use std::sync::Arc;
    use tokenless_ccr::InMemoryStore;

    let store = Arc::new(InMemoryStore::new());
    let compressor = SchemaCompressor::new()
        .with_func_desc_max_len(100)
        .with_stash_store(store.clone());
    for i in 0..3 {
        let schema = json!({
            "function": {
                "name": format!("f{i}"),
                "description": format!("DESC{i}_{}", "A".repeat(300)),
                "parameters": {"type": "object", "properties": {}}
            }
        });
        let _ = compressor.compress(&schema);
    }
    assert_eq!(store.len(), 3);
    assert_eq!(compressor.rollback_stash_writes(), 3);
    assert_eq!(store.len(), 0);
}

#[test]
fn test_rollback_updates_generation_after_same_description_refresh() {
    // Two schemas with the same long description stash the same payload twice
    // in one session. Rollback must use the refreshed generation.
    use std::sync::Arc;
    use tokenless_ccr::InMemoryStore;

    let store = Arc::new(InMemoryStore::new());
    let compressor = SchemaCompressor::new()
        .with_func_desc_max_len(100)
        .with_stash_store(store.clone());
    let desc = "A".repeat(300);
    for name in ["f1", "f2"] {
        let schema = json!({
            "function": {
                "name": name,
                "description": desc,
                "parameters": {"type": "object", "properties": {}}
            }
        });
        let _ = compressor.compress(&schema);
    }
    assert_eq!(store.len(), 1);
    assert_eq!(
        compressor.stash_writes(),
        1,
        "in-session refresh of the same key must not double-count stash_writes"
    );
    assert_eq!(compressor.rollback_stash_writes(), 1);
    assert_eq!(store.len(), 0);
    assert_eq!(compressor.stash_writes(), 0);
}

#[test]
fn test_rollback_does_not_re_adopt_after_intervening_foreign_refresh() {
    // A creates the row, B refreshes it and emits a marker, then A stashes
    // the same payload again. Re-adopting B's generation would make A's
    // rollback delete the row B's marker still needs.
    use std::sync::Arc;
    use tokenless_ccr::{InMemoryStore, StashStore, extract_hash};

    let store = Arc::new(InMemoryStore::new());
    let a = SchemaCompressor::new()
        .with_func_desc_max_len(100)
        .with_stash_store(store.clone());
    let b = SchemaCompressor::new()
        .with_func_desc_max_len(100)
        .with_stash_store(store.clone());
    let desc = "A".repeat(300);
    let schema = |name: &str| {
        json!({
            "function": {
                "name": name,
                "description": desc,
                "parameters": {"type": "object", "properties": {}}
            }
        })
    };
    let _ = a.compress(&schema("f1"));
    let emitted = b.compress(&schema("f2"));
    let hash = extract_hash(
        emitted["function"]["description"]
            .as_str()
            .expect("truncated description"),
    )
    .expect("B marker");
    assert_eq!(
        store.retrieve(hash).unwrap().as_deref(),
        Some(desc.as_str()),
        "B's marker must be retrievable before A's re-stash"
    );
    let _ = a.compress(&schema("f3"));
    assert_eq!(
        store.retrieve(hash).unwrap().as_deref(),
        Some(desc.as_str()),
        "B's marker must stay retrievable after A's re-stash"
    );
    let removed = a.rollback_stash_writes();
    assert_eq!(
        removed, 0,
        "A must not re-adopt a key after an intervening foreign refresh"
    );
    assert_eq!(
        store.retrieve(hash).unwrap().as_deref(),
        Some(desc.as_str()),
        "B's emitted marker must remain retrievable after A's rollback"
    );
}

#[test]
fn test_rollback_does_not_re_adopt_after_foreign_refresh_across_sqlite_connections() {
    // Same interleaving as the in-memory test, with two independent
    // SqliteStore connections on one file (CLI processes share stash.db).
    use std::sync::Arc;
    use tokenless_ccr::{SqliteStore, StashStore, extract_hash};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stash.db");
    let store_a = Arc::new(SqliteStore::new(&path).unwrap());
    let store_b = Arc::new(SqliteStore::new(&path).unwrap());
    let a = SchemaCompressor::new()
        .with_func_desc_max_len(100)
        .with_stash_store(store_a.clone());
    let b = SchemaCompressor::new()
        .with_func_desc_max_len(100)
        .with_stash_store(store_b.clone());
    let desc = "A".repeat(300);
    let schema = |name: &str| {
        json!({
            "function": {
                "name": name,
                "description": desc,
                "parameters": {"type": "object", "properties": {}}
            }
        })
    };
    let _ = a.compress(&schema("f1"));
    let emitted = b.compress(&schema("f2"));
    let hash = extract_hash(
        emitted["function"]["description"]
            .as_str()
            .expect("truncated description"),
    )
    .expect("B marker");
    assert!(store_b.retrieve(hash).unwrap().is_some());
    let _ = a.compress(&schema("f3"));
    assert!(store_b.retrieve(hash).unwrap().is_some());
    let removed = a.rollback_stash_writes();
    assert_eq!(removed, 0);
    assert_eq!(
        store_b.retrieve(hash).unwrap().as_deref(),
        Some(desc.as_str())
    );
}

#[test]
fn test_clear_stash_session_keeps_emitted_markers() {
    // SchemaCompressor accumulates keys across compress(). After emitting
    // one result, clear_stash_session() must drop it from the pending list
    // so a later rollback cannot delete that marker.
    use std::sync::Arc;
    use tokenless_ccr::{InMemoryStore, StashStore, extract_hash};

    let store = Arc::new(InMemoryStore::new());
    let compressor = SchemaCompressor::new()
        .with_func_desc_max_len(100)
        .with_stash_store(store.clone());
    let desc_keep = "K".repeat(300);
    let desc_discard = "D".repeat(300);
    let keep = json!({
        "function": {
            "name": "keep",
            "description": desc_keep,
            "parameters": {"type": "object", "properties": {}}
        }
    });
    let discard = json!({
        "function": {
            "name": "discard",
            "description": desc_discard,
            "parameters": {"type": "object", "properties": {}}
        }
    });
    let kept = compressor.compress(&keep);
    let keep_hash = extract_hash(
        kept["function"]["description"]
            .as_str()
            .expect("truncated description"),
    )
    .expect("keep marker");
    assert_eq!(store.len(), 1);
    compressor.clear_stash_session();
    let _ = compressor.compress(&discard);
    assert_eq!(store.len(), 2);
    assert_eq!(compressor.rollback_stash_writes(), 1);
    assert_eq!(store.len(), 1);
    assert_eq!(
        store.retrieve(keep_hash).unwrap().as_deref(),
        Some(desc_keep.as_str()),
        "emitted keep-marker payload must survive rollback after clear_stash_session"
    );
}

#[test]
fn test_gemini_wrapper_multi_declaration_order_and_titles() {
    // Multi-declaration wrappers keep declaration names and order, and
    // declaration-level titles are dropped like in the OpenAI wrapper.
    let compressor = SchemaCompressor::new();
    let tool = json!({
        "functionDeclarations": [
            {
                "name": "shell",
                "description": "Run a shell command in the workspace. ".repeat(20),
                "title": "Shell",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The command line to execute. ".repeat(12)
                        }
                    },
                    "required": ["command"]
                }
            },
            {
                "name": "read_file",
                "description": "Read a file. ".repeat(25),
                "parameters": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}}
                }
            }
        ]
    });

    let result = compressor.compress(&tool);

    let decls = result["functionDeclarations"].as_array().unwrap();
    assert_eq!(decls.len(), 2);
    assert_eq!(decls[0]["name"], "shell");
    assert_eq!(decls[1]["name"], "read_file");
    // Declaration titles dropped.
    assert!(decls[0].get("title").is_none());
    // Protected schema fields preserved.
    assert_eq!(decls[0]["parameters"]["required"][0], "command");
    // The rewrite actually shrank the declaration set.
    assert!(
        serde_json::to_string(&result).unwrap().len() < serde_json::to_string(&tool).unwrap().len()
    );
}

#[test]
fn test_gemini_malformed_function_declarations_untouched() {
    // A non-array functionDeclarations value is not a valid wrapper and
    // must pass through unchanged.
    let compressor = SchemaCompressor::new();
    let malformed = json!({"functionDeclarations": {"name": "not-an-array"}});
    assert_eq!(compressor.compress(&malformed), malformed);
}

#[test]
fn test_gemini_wrapper_no_savings_returns_original() {
    let compressor = SchemaCompressor::new();
    let tool = json!({
        "functionDeclarations": [
            {
                "name": "shell",
                "description": "Run a shell command.",
                "parameters": {"type": "object", "properties": {}}
            }
        ]
    });

    // Nothing to compress: the original value is returned unchanged.
    assert_eq!(compressor.compress(&tool), tool);
}
