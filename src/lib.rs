//! Go parser plugin — full-parse mode.
//!
//! Handles `.go` files.  The host parses source with tree-sitter-go
//! and sends the CST as JSON.

use intentumdiff_plugin_sdk::{
    cst::CstNode,
    hash::structural_hash_with_memo,
    tree::{SemanticNode, SemanticNodeBuilder},
};

wit_bindgen::generate!({
    path: "wit/plugin.wit",
    world: "parser-plugin",
});

use crate::exports::intentdiff::plugin::parser::ExamplePair;
use crate::exports::intentdiff::plugin::parser::Guest;
use crate::exports::intentdiff::plugin::parser::LanguageInfoRecord;
use crate::exports::intentdiff::plugin::parser::ParserMode;

const PLUGIN_METADATA: &str = include_str!("../plugin_metadata.info");

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
struct GoParser;

const TRIVIA: &[&str] = &["comment", "line_comment", "block_comment", "whitespace"];

const SEMANTIC_TYPES: &[&str] = &[
    // Root
    "source_file",
    // Declarations
    "function_declaration",
    "method_declaration",
    "type_declaration",
    "type_spec",
    "var_declaration",
    "var_spec",
    "const_declaration",
    "const_spec",
    "short_var_declaration",
    // Composite types
    "struct_type",
    "interface_type",
    "field_declaration",
    "method_spec",
    // Import / package
    "import_declaration",
    "import_spec",
    "package_clause",
    // Statements
    "expression_statement",
    "return_statement",
    "if_statement",
    "for_statement",
    "range_clause",
    "select_statement",
    "communication_case",
    "switch_statement",
    "expression_switch_statement",
    "type_switch_statement",
    "defer_statement",
    "go_statement",
    "send_statement",
    "inc_statement",
    "dec_statement",
    "assignment_statement",
    "labeled_statement",
    "break_statement",
    "continue_statement",
    "goto_statement",
    "fallthrough_statement",
    // Expressions
    "call_expression",
    "func_literal",
    // Identifiers / literals
    "identifier",
    "type_identifier",
    "field_identifier",
    "interpreted_string_literal",
    "raw_string_literal",
    "int_literal",
    "true",
    "false",
    "nil",
];

fn is_semantic(node_type: &str) -> bool {
    SEMANTIC_TYPES.contains(&node_type)
}

fn label_for(node: &CstNode) -> String {
    if node.is_leaf() {
        return node.text_or_empty().to_string();
    }
    // Literal containers label with their captured source text (SDK-shared, issue #47).
    if let Some(label) = intentumdiff_plugin_sdk::ts_convert::literal_label(node) {
        return label;
    }
    match node.node_type.as_str() {
        "function_declaration" | "type_spec" | "var_spec" | "const_spec" => {
            for child in &node.children {
                if child.node_type == "identifier" || child.node_type == "type_identifier" {
                    return child.text_or_empty().to_string();
                }
            }
        }
        // method_declaration: (receiver) name params — grab identifier after the parameter_list
        "method_declaration" => {
            let mut past_receiver = false;
            for child in &node.children {
                if child.node_type == "parameter_list" {
                    past_receiver = true;
                    continue;
                }
                if past_receiver && child.node_type == "field_identifier" {
                    return child.text_or_empty().to_string();
                }
                if past_receiver && child.node_type == "identifier" {
                    return child.text_or_empty().to_string();
                }
            }
        }
        "import_spec" => {
            for child in &node.children {
                if child.node_type == "interpreted_string_literal"
                    || child.node_type == "raw_string_literal"
                {
                    return child.text_or_empty().to_string();
                }
            }
        }
        _ => {}
    }
    for child in &node.children {
        if child.node_type == "identifier" || child.node_type == "type_identifier" {
            return child.text_or_empty().to_string();
        }
    }
    node.node_type.clone()
}

// Go has no classical class inheritance; structs/interfaces are the closest.
fn is_class_like(node_type: &str) -> bool {
    matches!(node_type, "struct_type" | "interface_type")
}

fn is_method_like(node_type: &str) -> bool {
    matches!(
        node_type,
        "function_declaration" | "method_declaration" | "func_literal"
    )
}

fn convert(
    node: &CstNode,
    id_prefix: &str,
    parent_class: Option<&str>,
    memo: &mut std::collections::HashMap<usize, String>,
) -> Option<SemanticNode> {
    convert_semantic_classed(
        node,
        id_prefix,
        parent_class,
        memo,
        &|_| false,
        &is_semantic,
        &is_class_like,
        &is_method_like,
        &label_for,
    )
}



use intentumdiff_plugin_sdk::ts_convert::{convert_semantic_classed, node_to_cst};

fn parse_source(source: &str) -> Result<CstNode, String> {
    let mut parser = tree_sitter::Parser::new();
    let lang = tree_sitter_go::LANGUAGE.into();
    parser
        .set_language(&lang)
        .map_err(|_| "Failed to load go grammar".to_string())?;
    let tree = parser
        .parse(source, None)
        .ok_or_else(|| "Parse failed".to_string())?;
    Ok(node_to_cst(tree.root_node(), source.as_bytes()))
}

fn process_impl(source: &str) -> String {
    let root: CstNode = match parse_source(source) {
        Ok(n) => n,
        Err(e) => return format!(r#"{{\"error\":\"{}\"}}"#, e),
    };
    let mut memo: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
    let sem = match convert(&root, "0", None, &mut memo) {
        Some(n) => n,
        None => return r#"{"error":"Empty semantic tree"}"#.to_string(),
    };
    match serde_json::to_string(&sem) {
        Ok(s) => s,
        Err(e) => format!(r#"{{"error":"Serialisation error: {}"}}"#, e),
    }
}

impl Guest for GoParser {
    fn get_parser_mode() -> ParserMode {
        ParserMode::FullParse
    }
    fn grammar_id() -> String {
        "go".to_string()
    }
    fn detect_language(filename: String, _content: String) -> String {
        if filename.ends_with(".go") {
            "go".to_string()
        } else {
            String::new()
        }
    }
    fn preprocess_source(source: String) -> String {
        source
    }
    fn example(_language: String) -> ExamplePair {
        ExamplePair {
            old: "package main\n\nimport \"fmt\"\n\nfunc add(a, b int) int {\n\treturn a + b\n}\n\nfunc main() {\n\tresult := add(3, 4)\n\tfmt.Println(result)\n}\n".to_string(),
            new: "package main\n\nimport \"fmt\"\n\nfunc add(x, y int) int {\n\treturn x + y\n}\n\nfunc subtract(x, y int) int {\n\treturn x - y\n}\n\nfunc main() {\n\tfmt.Println(add(3, 4))\n\tfmt.Println(subtract(10, 3))\n}\n".to_string(),
        }
    }
    fn process(input: String, _language: String, _filename: String) -> String {
        process_impl(&input)
    }
    fn trivia_node_types() -> Vec<String> {
        TRIVIA.iter().map(|s| s.to_string()).collect()
    }
    fn language_ids() -> Vec<String> {
        vec!["go".to_string()]
    }
    fn language_info() -> Vec<LanguageInfoRecord> {
        language_info_for(Self::language_ids())
    }
    fn priority() -> i32 {
        0
    }
}

export!(GoParser);

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exports::intentdiff::plugin::parser::Guest;
    use intentumdiff_plugin_sdk::testing as t;

    #[test]
    fn grammar_id_nonempty() {
        assert!(!GoParser::grammar_id().is_empty());
    }

    #[test]
    fn language_ids_contain_grammar_id() {
        let gid = GoParser::grammar_id();
        let ids = GoParser::language_ids();
        assert!(
            ids.contains(&gid),
            "language_ids {:?} must contain {:?}",
            ids,
            gid
        );
    }

    #[test]
    fn detect_language_known_ext() {
        let r = GoParser::detect_language("test.go".to_string(), "".to_string());
        assert_eq!(r.as_str(), "go");
    }

    #[test]
    fn detect_language_unknown_ext() {
        let r = GoParser::detect_language("test.xyz_notareal_ext_9z8y".to_string(), "".to_string());
        assert_eq!(r.as_str(), "");
    }

    #[test]
    fn parser_mode_is_full_parse() {
        assert!(matches!(GoParser::get_parser_mode(), ParserMode::FullParse));
    }

    #[test]
    fn process_impl_accepts_raw_example_source() {
        let example = GoParser::example(GoParser::grammar_id());
        let out = process_impl(&example.old);
        t::assert_valid_json(&out, "process(raw example)");
        assert!(!out.contains("\"error\""), "{out}");
    }
    #[test]
    fn process_impl_empty_returns_valid_json() {
        let out = process_impl("");
        t::assert_valid_json(&out, "process(empty)");
    }

    #[test]
    fn process_impl_whitespace_returns_valid_json() {
        let out = process_impl("   \n  ");
        t::assert_valid_json(&out, "process(whitespace)");
    }
}
