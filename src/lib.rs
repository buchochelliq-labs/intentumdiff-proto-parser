//! Protocol Buffers parser plugin — full-parse mode on tree-sitter-proto (issue #48).
//! Review identity lives in message/service/enum/rpc NAMES and field identifiers:
//! containers are labeled by their name node, fields by their identifier with the
//! field number and type as semantic children — a type or number change pairs as a
//! MODIFICATION under the stable field identity (the wire-compat review story).

use intentumdiff_plugin_sdk::{
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
    // Tree-sitter recovery nodes must retain the original edited bytes. In
    // particular, `syntax = "proto3X";` is not a valid proto syntax version,
    // but silently pruning its ERROR node produced an empty new-side change.
    "ERROR",
];

fn is_semantic(node_type: &str) -> bool {
    SEMANTIC_TYPES.contains(&node_type)
}

fn language_info_for(ids: Vec<String>) -> Vec<LanguageInfoRecord> {
    let metadata = intentumdiff_plugin_sdk::metadata::parse_plugin_metadata(PLUGIN_METADATA);
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
    use intentumdiff_plugin_sdk::tree::SemanticNode;

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
        intentumdiff_plugin_sdk::testing::assert_valid_json(&parsed, LANGUAGE_ID);
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

    fn descendant_covers_byte(node: &SemanticNode, row: u32, col: u32) -> bool {
        node.children.iter().any(|child| {
            let pos = &child.position;
            ((pos.start_line, pos.start_col) <= (row, col)
                && (row, col) < (pos.end_line, pos.end_col))
                || descendant_covers_byte(child, row, col)
        })
    }

    #[test]
    fn invalid_syntax_string_edit_preserves_new_side_source_evidence() {
        // The 74-language corpus inserts X just before the closing quote in
        // the top-level syntax directive. Proto's tree-sitter grammar accepts
        // only the exact "proto2"/"proto3" literals; invalid editor input must
        // remain visible through an ERROR recovery node, not disappear.
        let original = "syntax = \"proto3\";\n\nmessage Invoice { string reference = 1; }\n";
        let edited = "syntax = \"proto3X\";\n\nmessage Invoice { string reference = 1; }\n";
        let old: SemanticNode = serde_json::from_str(&process_impl(original)).unwrap();
        let new: SemanticNode = serde_json::from_str(&process_impl(edited)).unwrap();

        assert!(
            descendant_covers_byte(&new, 0, 16),
            "edited X at UTF-8 byte col 16 must remain inside a non-root semantic node: {new:#?}"
        );
        assert!(
            descendant_covers_byte(&old, 0, 15),
            "unchanged original syntax must remain source-anchored: {old:#?}"
        );
        assert_ne!(
            old.structural_hash, new.structural_hash,
            "invalid syntax text must not be silently normalized or reduced to style"
        );
    }

    fn find_exact<'a>(
        node: &'a SemanticNode,
        span: (u32, u32, u32, u32),
    ) -> Option<&'a SemanticNode> {
        let p = &node.position;
        if (p.start_line, p.start_col, p.end_line, p.end_col) == span {
            return Some(node);
        }
        node.children.iter().find_map(|c| find_exact(c, span))
    }

    #[test]
    fn invalid_syntax_string_edit_keeps_exact_utf8_span_of_inserted_byte() {
        // Issue #4 acceptance: the edited side keeps a concrete, nonzero span covering
        // [row 0, byte columns 16-17) -- the inserted `X` -- and the old side keeps its
        // real `syntax` directive. Positions are 0-based rows and UTF-8 byte columns.
        let original = "syntax = \"proto3\";\n\nmessage Invoice { string reference = 1; }\n";
        let edited = "syntax = \"proto3X\";\n\nmessage Invoice { string reference = 1; }\n";
        let old: SemanticNode = serde_json::from_str(&process_impl(original)).unwrap();
        let new: SemanticNode = serde_json::from_str(&process_impl(edited)).unwrap();

        let x = find_exact(&new, (0, 16, 0, 17)).expect("node spanning exactly the inserted X");
        assert_eq!(x.label, "X");
        assert_eq!(&edited[16..17], "X");
        // The invalid directive is not normalized back into a valid `syntax` node.
        assert!(new.children.iter().all(|c| c.node_type != "syntax"), "{new:#?}");
        assert_eq!(new.children[0].node_type, "ERROR");

        let syntax = find_exact(&old, (0, 0, 0, 18)).expect("old syntax directive");
        assert_eq!(syntax.node_type, "syntax");
        assert_eq!(syntax.label, "syntax = \"proto3\";");

        // Deterministic, and the untouched message keeps its identity and hash.
        assert_eq!(process_impl(edited), process_impl(edited));
        assert_eq!(old.children[1].label, "Invoice");
        assert_eq!(old.children[1].structural_hash, new.children[1].structural_hash);
    }

    #[test]
    fn field_type_change_alters_the_root_hash() {
        let old: SemanticNode = serde_json::from_str(&process_impl(DEFAULT_OLD)).unwrap();
        let new: SemanticNode = serde_json::from_str(&process_impl(DEFAULT_NEW)).unwrap();
        assert_ne!(old.structural_hash, new.structural_hash);
    }
}
