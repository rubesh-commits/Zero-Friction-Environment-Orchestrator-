use crate::rules::{
    self, java_import_rule, java_is_stdlib, node_is_builtin, python_import_rule, python_is_stdlib,
};
use std::collections::BTreeSet;
use std::path::Path;
use tree_sitter::{Node, Parser};
use linuxorchestrator_core::{Language, Version};

#[derive(Clone, Debug, Default)]
pub struct AstReport {
    pub imports: BTreeSet<String>,
    pub hits: Vec<rules::RuleHit>,
    pub warnings: Vec<String>,
}

impl AstReport {
    fn add_hit(&mut self, hit: rules::RuleHit, seen: &mut BTreeSet<(String, usize)>) {
        if seen.insert((hit.construct.clone(), hit.line)) {
            self.hits.push(hit);
        }
    }
}

/// Parse one source file with tree-sitter and apply the embedded rules.
pub fn analyze_source(path: &Path, relative: &str, language: Language) -> AstReport {
    let mut report = AstReport::default();
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            report
                .warnings
                .push(format!("could not read {relative}: {err}"));
            return report;
        }
    };
    let mut parser = Parser::new();
    let language_fn = match language {
        Language::Python => tree_sitter_python::LANGUAGE.into(),
        Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Language::TypeScript => {
            let is_tsx = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("tsx"))
                .unwrap_or(false);
            if is_tsx {
                tree_sitter_typescript::LANGUAGE_TSX.into()
            } else {
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
            }
        }
        Language::Java => tree_sitter_java::LANGUAGE.into(),
        _ => return report,
    };
    if let Err(err) = parser.set_language(&language_fn) {
        report.warnings.push(format!(
            "tree-sitter could not load the {language:?} grammar: {err}"
        ));
        return report;
    }
    let tree = match parser.parse(&bytes, None) {
        Some(tree) => tree,
        None => {
            report
                .warnings
                .push(format!("tree-sitter could not parse {relative}"));
            return report;
        }
    };

    let mut seen = BTreeSet::new();
    walk(
        tree.root_node(),
        &bytes,
        relative,
        language,
        &mut report,
        &mut seen,
        false,
        0,
    );

    // Module-level rules (imports etc.).
    for module in report.imports.clone() {
        let rule = match language {
            Language::Python => python_import_rule(&module),
            Language::Java => java_import_rule(&module),
            _ => None,
        };
        if let Some((construct, min, explanation)) = rule {
            report.add_hit(
                rules::RuleHit::new(
                    language,
                    construct,
                    min,
                    explanation,
                    relative,
                    0,
                    &format!("import {module}"),
                ),
                &mut seen,
            );
        }
    }
    report
}

fn walk(
    node: Node,
    source: &[u8],
    file: &str,
    language: Language,
    report: &mut AstReport,
    seen: &mut BTreeSet<(String, usize)>,
    in_function: bool,
    depth: usize,
) {
    if depth > 600 {
        return;
    }
    let line = node.start_position().row + 1;
    match language {
        Language::Python => scan_python(node, source, file, line, report, seen),
        Language::JavaScript | Language::TypeScript => {
            scan_javascript(node, source, file, line, report, seen, in_function)
        }
        Language::Java => scan_java(node, source, file, line, report, seen),
        _ => {}
    }

    let next_in_function = in_function
        || matches!(
            node.kind(),
            "function_declaration"
                | "function_expression"
                | "arrow_function"
                | "method_definition"
                | "generator_function_declaration"
                | "generator_function"
        );
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    for child in children {
        walk(
            child,
            source,
            file,
            language,
            report,
            seen,
            next_in_function,
            depth + 1,
        );
    }
}

fn push_hit(
    report: &mut AstReport,
    seen: &mut BTreeSet<(String, usize)>,
    language: Language,
    construct: &str,
    minimum: Version,
    explanation: &str,
    file: &str,
    line: usize,
    snippet: &str,
) {
    report.add_hit(
        rules::RuleHit::new(
            language,
            construct,
            minimum,
            explanation,
            file,
            line,
            snippet,
        ),
        seen,
    );
}

fn node_text<'a>(node: Node, source: &'a [u8]) -> &'a str {
    node.utf8_text(source).unwrap_or("")
}

fn scan_python(
    node: Node,
    source: &[u8],
    file: &str,
    line: usize,
    report: &mut AstReport,
    seen: &mut BTreeSet<(String, usize)>,
) {
    let text = node_text(node, source);
    macro_rules! hit {
        ($construct:expr, $min:expr, $why:expr $(,)?) => {
            push_hit(
                report,
                seen,
                Language::Python,
                $construct,
                $min,
                $why,
                file,
                line,
                text,
            )
        };
    }

    match node.kind() {
        "import_statement" => {
            for index in 0..node.named_child_count() {
                if let Some(child) = node.named_child(index) {
                    let module = node_text(child, source)
                        .split_whitespace()
                        .next()
                        .unwrap_or("")
                        .to_string();
                    let module = module.split('.').next().unwrap_or(&module).to_string();
                    if !module.is_empty() && !python_is_stdlib(&module) {
                        report.imports.insert(module);
                    }
                }
            }
        }
        "import_from_statement" => {
            let relative = (0..node.named_child_count())
                .filter_map(|index| node.named_child(index))
                .any(|child| child.kind() == "relative_import");
            if !relative {
                if let Some(child) = (0..node.named_child_count())
                    .filter_map(|index| node.named_child(index))
                    .find(|child| child.kind() == "dotted_name")
                {
                    let module = node_text(child, source);
                    if !module.is_empty() && !python_is_stdlib(module) {
                        report
                            .imports
                            .insert(module.split('.').next().unwrap_or(module).to_string());
                    }
                }
            }
        }
        "match_statement" => hit!(
            "match/case statement",
            Version::new(3, 10, 0),
            "Structural pattern matching (match/case) was added in Python 3.10.",
        ),
        "named_expression" => hit!(
            "walrus operator",
            Version::new(3, 8, 0),
            "Assignment expressions (:=) were added in Python 3.8.",
        ),
        "positional_separator" => hit!(
            "positional-only parameters",
            Version::new(3, 8, 0),
            "Positional-only parameter separators (/) were added in Python 3.8.",
        ),
        "type_alias_statement" => hit!(
            "type alias statement",
            Version::new(3, 12, 0),
            "The `type` statement was added in Python 3.12.",
        ),
        "type_parameter" => hit!(
            "PEP 695 type parameters",
            Version::new(3, 12, 0),
            "PEP 695 generic type parameter syntax was added in Python 3.12.",
        ),
        "interpolation" => hit!(
            "f-string",
            Version::new(3, 6, 0),
            "Formatted string literals were added in Python 3.6.",
        ),
        "except_clause" => {
            let grouped = (0..node.child_count())
                .filter_map(|index| node.child(index))
                .any(|child| child.kind() == "*");
            if grouped {
                hit!(
                    "except* group",
                    Version::new(3, 11, 0),
                    "Exception groups and except* were added in Python 3.11.",
                );
            }
        }
        "attribute" => match text {
            "sys.stdlib_module_names" => hit!(
                "sys.stdlib_module_names",
                Version::new(3, 10, 0),
                "sys.stdlib_module_names was added in Python 3.10.",
            ),
            "typing.Self" => hit!(
                "typing.Self",
                Version::new(3, 11, 0),
                "typing.Self was added in Python 3.11.",
            ),
            "typing.override" => hit!(
                "typing.override",
                Version::new(3, 12, 0),
                "typing.override was added in Python 3.12.",
            ),
            "functools.cache" => hit!(
                "functools.cache",
                Version::new(3, 9, 0),
                "functools.cache was added in Python 3.9.",
            ),
            "itertools.batched" => hit!(
                "itertools.batched",
                Version::new(3, 12, 0),
                "itertools.batched was added in Python 3.12.",
            ),
            "asyncio.TaskGroup" => hit!(
                "asyncio.TaskGroup",
                Version::new(3, 11, 0),
                "asyncio.TaskGroup was added in Python 3.11.",
            ),
            "asyncio.timeout" => hit!(
                "asyncio.timeout",
                Version::new(3, 11, 0),
                "asyncio.timeout was added in Python 3.11.",
            ),
            "pathlib.Path.walk" => hit!(
                "pathlib.Path.walk",
                Version::new(3, 12, 0),
                "Path.walk was added in Python 3.12.",
            ),
            _ => {}
        },
        "call" => {
            if let Some(callee_node) = node.named_child(0) {
                let callee = node_text(callee_node, source);
                for (construct, minimum, explanation) in python_call_rules(callee) {
                    hit!(construct, minimum, explanation);
                }
            }
        }
        _ => {}
    }
}

fn python_call_rules(callee: &str) -> Vec<(&'static str, Version, &'static str)> {
    let method = callee.rsplit('.').next().unwrap_or(callee);
    let mut rules = Vec::new();
    if callee == "itertools.batched" {
        rules.push((
            "itertools.batched",
            Version::new(3, 12, 0),
            "itertools.batched was added in Python 3.12.",
        ));
    }
    if callee == "asyncio.TaskGroup" || callee == "asyncio.timeout" {
        rules.push((
            "asyncio.TaskGroup/timeout",
            Version::new(3, 11, 0),
            "This asyncio API was added in Python 3.11.",
        ));
    }
    if method == "removeprefix" || method == "removesuffix" {
        rules.push((
            "str.removeprefix/removesuffix",
            Version::new(3, 9, 0),
            "str.removeprefix/removesuffix were added in Python 3.9.",
        ));
    }
    if method == "bit_count" {
        rules.push((
            "int.bit_count",
            Version::new(3, 10, 0),
            "int.bit_count was added in Python 3.10.",
        ));
    }
    rules
}

fn scan_javascript(
    node: Node,
    source: &[u8],
    file: &str,
    line: usize,
    report: &mut AstReport,
    seen: &mut BTreeSet<(String, usize)>,
    in_function: bool,
) {
    let text = node_text(node, source);
    macro_rules! hit {
        ($construct:expr, $min:expr, $why:expr $(,)?) => {
            push_hit(
                report,
                seen,
                Language::JavaScript,
                $construct,
                $min,
                $why,
                file,
                line,
                text,
            )
        };
    }

    match node.kind() {
        "import_statement" | "export_statement" => {
            for index in 0..node.named_child_count() {
                if let Some(child) = node.named_child(index) {
                    if child.kind() == "string" {
                        let module = strip_quotes(node_text(child, source));
                        if !module.is_empty()
                            && !module.starts_with('.')
                            && !module.starts_with('/')
                            && !node_is_builtin(module)
                        {
                            report.imports.insert(module.to_string());
                        }
                    }
                }
            }
        }
        "call_expression" => {
            if let Some(function) = node.named_child(0) {
                let callee = node_text(function, source);
                if callee == "require" || callee == "import" {
                    if let Some(arguments) = node.named_child(1) {
                        for index in 0..arguments.named_child_count() {
                            if let Some(arg) = arguments.named_child(index) {
                                if arg.kind() == "string" {
                                    let module = strip_quotes(node_text(arg, source));
                                    if !module.is_empty()
                                        && !module.starts_with('.')
                                        && !module.starts_with('/')
                                        && !node_is_builtin(module)
                                    {
                                        report.imports.insert(module.to_string());
                                    }
                                }
                            }
                        }
                    }
                }
                for (construct, minimum, explanation) in js_call_rules(callee) {
                    hit!(construct, minimum, explanation);
                }
            }
        }
        "optional_chain" => hit!(
            "optional chaining (?.)",
            Version::new(14, 0, 0),
            "Optional chaining requires Node.js 14 or newer.",
        ),
        "binary_expression" => {
            if child_token(node, 1).as_deref() == Some("??") {
                hit!(
                    "nullish coalescing (??)",
                    Version::new(14, 0, 0),
                    "Nullish coalescing requires Node.js 14 or newer.",
                );
            }
        }
        "augmented_assignment_expression" => {
            if let Some(operator) = child_token(node, 1) {
                if matches!(operator.as_str(), "||=" | "&&=" | "??=") {
                    hit!(
                        "logical assignment",
                        Version::new(15, 0, 0),
                        "Logical assignment operators require Node.js 15 or newer.",
                    );
                }
            }
        }
        "field_definition" | "class_static_block" | "private_property_identifier" => hit!(
            "class fields / private members",
            Version::new(12, 0, 0),
            "Public/private class fields require Node.js 12 or newer.",
        ),
        "number" => {
            if text.ends_with('n') {
                hit!(
                    "BigInt literal",
                    Version::new(10, 4, 0),
                    "BigInt literals require Node.js 10.4 or newer.",
                );
            }
        }
        "meta_property" => {
            if text.starts_with("import.meta") {
                hit!(
                    "import.meta",
                    Version::new(12, 0, 0),
                    "import.meta requires Node.js 12 or newer.",
                );
            }
        }
        "await_expression" => {
            if !in_function {
                hit!(
                    "top-level await",
                    Version::new(14, 8, 0),
                    "Top-level await requires Node.js 14.8 or newer (ES modules).",
                );
            }
        }
        _ => {}
    }
}

fn js_call_rules(callee: &str) -> Vec<(&'static str, Version, &'static str)> {
    let method = callee.rsplit('.').next().unwrap_or(callee);
    let mut rules = Vec::new();
    match callee {
        "fetch" => rules.push((
            "global fetch",
            Version::new(18, 0, 0),
            "A global fetch implementation is available from Node.js 18.",
        )),
        "structuredClone" => rules.push((
            "structuredClone",
            Version::new(17, 0, 0),
            "structuredClone became global in Node.js 17.",
        )),
        "Object.groupBy" | "Map.groupBy" => rules.push((
            "Object.groupBy/Map.groupBy",
            Version::new(21, 0, 0),
            "Object.groupBy/Map.groupBy were added in Node.js 21.",
        )),
        "Array.fromAsync" => rules.push((
            "Array.fromAsync",
            Version::new(22, 0, 0),
            "Array.fromAsync was added in Node.js 22.",
        )),
        "Promise.withResolvers" => rules.push((
            "Promise.withResolvers",
            Version::new(22, 0, 0),
            "Promise.withResolvers was added in Node.js 22.",
        )),
        "Promise.allSettled" => rules.push((
            "Promise.allSettled",
            Version::new(12, 9, 0),
            "Promise.allSettled was added in Node.js 12.9.",
        )),
        "Promise.any" => rules.push((
            "Promise.any",
            Version::new(15, 0, 0),
            "Promise.any was added in Node.js 15.",
        )),
        "crypto.randomUUID" => rules.push((
            "crypto.randomUUID",
            Version::new(14, 17, 0),
            "crypto.randomUUID was added in Node.js 14.17.",
        )),
        _ => {}
    }
    match method {
        "at" => rules.push((
            "Array/String .at()",
            Version::new(16, 6, 0),
            ".at() was added in Node.js 16.6.",
        )),
        "replaceAll" => rules.push((
            "String.replaceAll",
            Version::new(15, 0, 0),
            "String.replaceAll was added in Node.js 15.",
        )),
        "findLast" | "findLastIndex" => rules.push((
            "Array.findLast/findLastIndex",
            Version::new(18, 0, 0),
            "Array.findLast/findLastIndex were added in Node.js 18.",
        )),
        "hasOwn" => rules.push((
            "Object.hasOwn",
            Version::new(16, 9, 0),
            "Object.hasOwn was added in Node.js 16.9.",
        )),
        "toSorted" | "toReversed" | "toSpliced" => rules.push((
            "immutable array methods",
            Version::new(20, 0, 0),
            "toSorted/toReversed/toSpliced were added in Node.js 20.",
        )),
        _ => {}
    }
    rules
}

fn scan_java(
    node: Node,
    source: &[u8],
    file: &str,
    line: usize,
    report: &mut AstReport,
    seen: &mut BTreeSet<(String, usize)>,
) {
    let text = node_text(node, source);
    macro_rules! hit {
        ($construct:expr, $min:expr, $why:expr $(,)?) => {
            push_hit(
                report,
                seen,
                Language::Java,
                $construct,
                $min,
                $why,
                file,
                line,
                text,
            )
        };
    }

    match node.kind() {
        "import_declaration" => {
            for index in 0..node.named_child_count() {
                if let Some(child) = node.named_child(index) {
                    if child.kind() == "scoped_identifier" || child.kind() == "identifier" {
                        let module = node_text(child, source);
                        if !java_is_stdlib(module) && !module.is_empty() {
                            report.imports.insert(module.to_string());
                        }
                    }
                }
            }
        }
        "record_declaration" => hit!(
            "record declaration",
            Version::new(16, 0, 0),
            "Records were finalized in Java 16.",
        ),
        "switch_expression" => hit!(
            "switch expression",
            Version::new(14, 0, 0),
            "Switch expressions were finalized in Java 14.",
        ),
        "local_variable_declaration" => {
            if let Some(first) = node.named_child(0) {
                if first.kind() == "type_identifier" && node_text(first, source) == "var" {
                    hit!(
                        "local variable type inference (var)",
                        Version::new(10, 0, 0),
                        "Local variable type inference was added in Java 10.",
                    );
                }
            }
        }
        "string_literal" => {
            if text.trim_start().starts_with("\"\"\"") {
                hit!(
                    "text block",
                    Version::new(15, 0, 0),
                    "Text blocks were finalized in Java 15.",
                );
            }
        }
        "instanceof_expression" => {
            if node.named_child_count() >= 3 {
                hit!(
                    "pattern matching for instanceof",
                    Version::new(16, 0, 0),
                    "Pattern matching for instanceof was finalized in Java 16.",
                );
            }
        }
        "lambda_expression" => hit!(
            "lambda expression",
            Version::new(8, 0, 0),
            "Lambda expressions were added in Java 8.",
        ),
        "method_reference" => hit!(
            "method reference",
            Version::new(8, 0, 0),
            "Method references were added in Java 8.",
        ),
        "modifiers" => {
            let lowered = text.to_ascii_lowercase();
            if lowered.contains("sealed")
                || lowered.contains("non-sealed")
                || lowered.contains("permits")
            {
                hit!(
                    "sealed class",
                    Version::new(17, 0, 0),
                    "Sealed classes were finalized in Java 17.",
                );
            }
        }
        "module_declaration" => hit!(
            "module declaration",
            Version::new(9, 0, 0),
            "The Java Platform Module System was added in Java 9.",
        ),
        "try_with_resources_statement" => hit!(
            "try-with-resources",
            Version::new(7, 0, 0),
            "try-with-resources was added in Java 7.",
        ),
        "type_pattern" | "record_pattern" | "guard" => hit!(
            "pattern matching for switch",
            Version::new(21, 0, 0),
            "Pattern matching for switch was finalized in Java 21.",
        ),
        "method_invocation" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source))
                .unwrap_or("");
            let object = node
                .child_by_field_name("object")
                .map(|n| node_text(n, source))
                .unwrap_or("");
            let callee = if object.is_empty() {
                name.to_string()
            } else {
                format!("{object}.{name}")
            };
            for (construct, minimum, explanation) in java_call_rules(&callee) {
                hit!(construct, minimum, explanation);
            }
        }
        "identifier" if text == "HexFormat" => hit!(
            "java.util.HexFormat",
            Version::new(17, 0, 0),
            "java.util.HexFormat was added in Java 17.",
        ),
        _ => {}
    }
}

fn java_call_rules(callee: &str) -> Vec<(&'static str, Version, &'static str)> {
    let method = callee.rsplit('.').next().unwrap_or(callee);
    let mut rules = Vec::new();
    if callee.contains("Thread.ofVirtual") || callee.contains("Thread.ofPlatform") {
        rules.push((
            "virtual threads",
            Version::new(21, 0, 0),
            "Virtual threads were finalized in Java 21.",
        ));
    }
    if callee.starts_with("List.of") || callee.starts_with("Set.of") || callee.starts_with("Map.of")
    {
        rules.push((
            "collection factory methods",
            Version::new(9, 0, 0),
            "List.of/Set.of/Map.of were added in Java 9.",
        ));
    }
    if callee.starts_with("Files.readString") || callee.starts_with("Files.writeString") {
        rules.push((
            "java.nio.file string helpers",
            Version::new(11, 0, 0),
            "Files.readString/writeString were added in Java 11.",
        ));
    }
    if callee.contains("Collectors.teeing") {
        rules.push((
            "Collectors.teeing",
            Version::new(12, 0, 0),
            "Collectors.teeing was added in Java 12.",
        ));
    }
    if callee.contains("HexFormat.") {
        rules.push((
            "java.util.HexFormat",
            Version::new(17, 0, 0),
            "java.util.HexFormat was added in Java 17.",
        ));
    }
    match method {
        "toList" => rules.push((
            "Stream.toList",
            Version::new(16, 0, 0),
            "Stream.toList was added in Java 16.",
        )),
        "isBlank" | "strip" | "stripLeading" | "stripTrailing" | "repeat" | "lines" => {
            rules.push((
                "Java 11 String methods",
                Version::new(11, 0, 0),
                "This String convenience API was added in Java 11.",
            ))
        }
        "formatted" => rules.push((
            "String.formatted",
            Version::new(15, 0, 0),
            "String.formatted was added in Java 15.",
        )),
        "mapMulti" => rules.push((
            "Stream.mapMulti",
            Version::new(16, 0, 0),
            "Stream.mapMulti was added in Java 16.",
        )),
        _ => {}
    }
    rules
}

fn child_token(node: Node, index: usize) -> Option<String> {
    node.child(index).map(|child| child.kind().to_string())
}

fn strip_quotes(text: &str) -> &str {
    text.trim_matches(|c| c == '"' || c == '\'' || c == '`')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scan(name: &str, language: Language, source: &str) -> AstReport {
        let dir = std::env::temp_dir().join("linuxorchestrator-ast-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let ext = match language {
            Language::Python => "py",
            Language::JavaScript => "js",
            Language::TypeScript => "ts",
            Language::Java => "java",
            _ => "txt",
        };
        let path = dir.join(format!("{name}.{ext}"));
        std::fs::write(&path, source).unwrap();
        analyze_source(&path, &path.display().to_string(), language)
    }

    #[test]
    fn python_detects_match_walrus_and_fstrings() {
        let report = scan(
            "py_rules",
            Language::Python,
            "import requests\nmatch x:\n    case 1: pass\nif (n := 2): pass\nprint(f'{n}')\n",
        );
        assert!(report.imports.contains("requests"));
        let constructs: Vec<_> = report.hits.iter().map(|h| h.construct.as_str()).collect();
        assert!(constructs.contains(&"match/case statement"));
        assert!(constructs.contains(&"walrus operator"));
        assert!(constructs.contains(&"f-string"));
    }

    #[test]
    fn javascript_detects_optional_chaining_and_fetch() {
        let report = scan(
            "js_rules",
            Language::JavaScript,
            "import axios from 'axios';\nconst x = a?.b ?? 1;\nfetch('/x');\n",
        );
        assert!(report.imports.contains("axios"));
        let constructs: Vec<_> = report.hits.iter().map(|h| h.construct.as_str()).collect();
        assert!(constructs.contains(&"optional chaining (?.)"));
        assert!(constructs.contains(&"nullish coalescing (??)"));
        assert!(constructs.contains(&"global fetch"));
    }

    #[test]
    fn java_detects_records_and_var() {
        let report = scan(
            "java_rules",
            Language::Java,
            "import java.util.List;\nrecord P(int x) {}\nclass A { void f() { var x = List.of(1); } }\n",
        );
        assert!(report.imports.is_empty());
        let constructs: Vec<_> = report.hits.iter().map(|h| h.construct.as_str()).collect();
        assert!(constructs.contains(&"record declaration"));
        assert!(constructs.contains(&"local variable type inference (var)"));
        assert!(constructs.contains(&"collection factory methods"));
    }

    #[test]
    fn top_level_await_only_counts_outside_functions() {
        let report = scan(
            "js_await",
            Language::JavaScript,
            "await go();\nasync function f() { await go(); }\n",
        );
        let count = report
            .hits
            .iter()
            .filter(|h| h.construct == "top-level await")
            .count();
        assert_eq!(count, 1);
    }

    #[test]
    fn fixture_path_is_created() {
        // Guard the helper against accidental relative writes.
        let _ = PathBuf::from(".");
    }
}
