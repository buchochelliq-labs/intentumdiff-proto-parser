//! Protocol Buffers parser plugin — full-parse mode on tree-sitter-proto (issue #48).
//! Review identity lives in message/service/enum/rpc NAMES and field identifiers:
//! containers are labeled by their name node, fields by their identifier with the
//! field number and type as semantic children — a type or number change pairs as a
//! MODIFICATION under the stable field identity (the wire-compat review story).

use intentdiff_plugin_sdk::{
    cst::CstNode,
    ts_convert::{convert_semantic, node_to_cst},
    tree::SemanticNodeBuilder,
};

wit_bindgen::generate!({
    path: "wit/plugin.wit",
    world: "parser-plugin",
});

use crate::exports::intentdiff::plugin::parser::ExamplePair;
use crate::exports::intentdiff::plugin::parser::Guest;
use crate::exports::intentdiff::plugin::parser::LanguageInfoRecord;
use crate::exports::intentdiff::plugin::parser::ParserMode;

const LANGUAGE_ID: &str = "proto";
const PLUGIN_METADATA: &str = include_str!("../plugin_metadata.info");

const DEFAULT_OLD: &str = "syntax = \"proto3\";\n\nmessage User {\n  string name = 1;\n  int32 login_count = 2;\n}\n";
const DEFAULT_NEW: &str = "syntax = \"proto3\";\n\nmessage User {\n  string name = 1;\n  int64 login_count = 2;\n  bool active = 3;\n}\n";

// Declarations, fields and their identity/type/number parts carry review meaning;
// braces, semicolons and comments are dropped (not listed, no semantic children).
const SEMANTIC_TYPES: &[&str] = &[
    "source_file",
    "syntax",
    "edition",
    "package",
    "import",
    "option",
    "message",
    "message_body",
    "enum",
    "enum_body",
    "enum_field",
    "service",
    "rpc",
    "field",
    "map_field",
    "oneof",
    "oneof_field",
    "type",
    "key_type",
    "field_number",
    "message_or_enum_type",
];

fn is_semantic(node_type: &str) -> bool {
    SEMANTIC_TYPES.contains(&node_type)
}

fn language_info_for(ids: Vec<String>) -> Vec<LanguageInfoRecord> {
    let metadata = intentdiff_plugin_sdk::metadata::parse_plugin_metadata(PLUGIN_METADATA);
    ids.into_iter()
        .map(|language_id| {
            let info = metadata.language_or_default(&language_id);
            LanguageInfoRecord {
                language_id: info.language_id,
                language_name: info.language_name,
                language_short_name: info.language_short_name,
                monaco_language: info.monaco_language,
                default_filename: info.default_filename,
                language_file_extensions: info.language_file_extensions,
                author: metadata.author().to_string(),
                plugin_version: metadata.plugin_version().to_string(),
                last_updated: metadata.last_updated().to_string(),
            }
        })
        .collect()
}

fn basename(path: &str) -> &str {
    path.rsplit(|ch| ch == '/' || ch == '\\')
        .next()
        .unwrap_or(path)
}

fn detect_language_impl(filename: &str, _content: &str) -> String {
    if basename(filename).to_lowercase().ends_with(".proto") {
        LANGUAGE_ID.to_string()
    } else {
        String::new()
    }
}

/// First non-empty LEAF text under `node` (CstNode only carries text on leaves).
fn leaf_text(node: &CstNode) -> Option<String> {
    if node.is_leaf() {
        let text = node.text_or_empty().trim();
        if !text.is_empty() {
            return Some(text.chars().take(120).collect());
        }
        return None;
    }
    node.children.iter().find_map(leaf_text)
}

/// First descendant of `key_type`, read via its leaves.
fn key_text(node: &CstNode, key_type: &str) -> Option<String> {
    fn find_key(node: &CstNode, key_type: &str) -> Option<String> {
        if node.node_type == key_type {
            if let Some(text) = leaf_text(node) {
                return Some(text);
            }
        }
        for child in &node.children {
            if let Some(text) = find_key(child, key_type) {
                return Some(text);
            }
        }
        None
    }
    find_key(node, key_type)
}

fn label_for(node: &CstNode) -> String {
    if node.is_leaf() {
        return node.text_or_empty().trim().chars().take(120).collect();
    }
    match node.node_type.as_str() {
        "message" => key_text(node, "message_name").unwrap_or_else(|| node.node_type.clone()),
        "enum" => key_text(node, "enum_name").unwrap_or_else(|| node.node_type.clone()),
        "service" => key_text(node, "service_name").unwrap_or_else(|| node.node_type.clone()),
        "rpc" => key_text(node, "rpc_name").unwrap_or_else(|| node.node_type.clone()),
        // A field's identity is its identifier; type and number stay as children.
        "field" | "map_field" | "oneof_field" | "enum_field" | "oneof" => {
            key_text(node, "identifier").unwrap_or_else(|| node.node_type.clone())
        }
        "package" => leaf_text(node).unwrap_or_else(|| node.node_type.clone()),
        "import" | "syntax" | "type" | "key_type" | "field_number"
        | "message_or_enum_type" => leaf_text(node).unwrap_or_else(|| node.node_type.clone()),
        _ => node.node_type.clone(),
    }
}

fn parse_source(source: &str) -> Result<CstNode, String> {
    let mut parser = tree_sitter::Parser::new();
    let lang = tree_sitter_proto::LANGUAGE.into();
    parser
        .set_language(&lang)
        .map_err(|_| "Failed to load proto grammar".to_string())?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| "tree-sitter failed to parse proto".to_string())?;
    Ok(node_to_cst(tree.root_node(), source.as_bytes()))
}

fn process_impl(source: &str) -> String {
    let cst = match parse_source(source) {
        Ok(cst) => cst,
        Err(err) => return format!(r#"{{"error":"{}"}}"#, err),
    };
    let mut memo = std::collections::HashMap::new();
    let node = convert_semantic(&cst, "0", &mut memo, &is_semantic, &label_for).unwrap_or_else(|| {
        SemanticNodeBuilder::new("0", "source_file", LANGUAGE_ID, 0, 0, 0, 0, "0").build()
    });
    match serde_json::to_string(&node) {
        Ok(serialized) => serialized,
        Err(err) => format!(r#"{{"error":"Serialisation error: {}"}}"#, err),
    }
}

struct ProtoParser;

impl Guest for ProtoParser {
    fn get_parser_mode() -> ParserMode {
        ParserMode::FullParse
    }

    fn grammar_id() -> String {
        LANGUAGE_ID.to_string()
    }

    fn detect_language(filename: String, content: String) -> String {
        detect_language_impl(&filename, &content)
    }

    fn preprocess_source(source: String) -> String {
        source
    }

    fn example(_language: String) -> ExamplePair {
        ExamplePair {
            old: DEFAULT_OLD.to_string(),
            new: DEFAULT_NEW.to_string(),
        }
    }

    fn process(input: String, _language: String, _filename: String) -> String {
        process_impl(&input)
    }

    fn trivia_node_types() -> Vec<String> {
        vec![]
    }

    fn language_ids() -> Vec<String> {
        vec![LANGUAGE_ID.to_string()]
    }

    fn language_info() -> Vec<LanguageInfoRecord> {
        language_info_for(Self::language_ids())
    }

    fn priority() -> i32 {
        5
    }
}

export!(ProtoParser);

#[cfg(test)]
mod tests {
    use super::*;
    use intentdiff_plugin_sdk::tree::SemanticNode;

    fn labels_by_type(node: &SemanticNode, node_type: &str, out: &mut Vec<String>) {
        if node.node_type == node_type {
            out.push(node.label.clone());
        }
        for child in &node.children {
            labels_by_type(child, node_type, out);
        }
    }

    #[test]
    fn parser_mode_is_full_parse() {
        assert_eq!(ProtoParser::get_parser_mode(), ParserMode::FullParse);
    }

    #[test]
    fn detects_proto_extension(){
        assert_eq!(detect_language_impl("service.proto", ""), LANGUAGE_ID);
        assert_eq!(detect_language_impl("api/v1/user.proto", ""), LANGUAGE_ID);
        assert_eq!(detect_language_impl("main.rs", ""), "");
    }

    #[test]
    fn messages_and_fields_are_labeled_by_identity() {
        let parsed = process_impl(DEFAULT_NEW);
        intentdiff_plugin_sdk::testing::assert_valid_json(&parsed, LANGUAGE_ID);
        let root: SemanticNode = serde_json::from_str(&parsed).unwrap();
        let mut messages = Vec::new();
        labels_by_type(&root, "message", &mut messages);
        assert_eq!(messages, vec!["User".to_string()], "messages: {messages:?}");
        let mut fields = Vec::new();
        labels_by_type(&root, "field", &mut fields);
        assert!(fields.contains(&"name".to_string()), "fields: {fields:?}");
        assert!(fields.contains(&"login_count".to_string()), "fields: {fields:?}");
        assert!(fields.contains(&"active".to_string()), "fields: {fields:?}");
    }

    #[test]
    fn field_type_change_alters_the_root_hash() {
        let old: SemanticNode = serde_json::from_str(&process_impl(DEFAULT_OLD)).unwrap();
        let new: SemanticNode = serde_json::from_str(&process_impl(DEFAULT_NEW)).unwrap();
        assert_ne!(old.structural_hash, new.structural_hash);
    }
}
