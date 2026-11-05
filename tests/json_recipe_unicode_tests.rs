//! Issue #198: JSON escapes must preserve recipe text and parser validation.
//! These fixtures are authored independently; neither is generated from the other.

use recipe_runner_rs::models::{Recipe, StepType};
use recipe_runner_rs::parser::{ParseError, RecipeParser};
use recipe_runner_rs::{parse_recipe, validate_recipe};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Command;

const RAW: &str = r#"{
  "name": "unicode",
  "description": "before 🚀🧪 after\n\"quoted\" C:\\tmp\\uD83D\\uDE80 literal \\n",
  "context": {"text": "before 🚀🧪 after\n\"quoted\" C:\\tmp\\uD83D\\uDE80 literal \\n"},
  "steps": [{"id": "ask", "prompt": "before 🚀🧪 after\n\"quoted\" C:\\tmp\\uD83D\\uDE80 literal \\n",
    "context": {"text": "before 🚀🧪 after\n\"quoted\" C:\\tmp\\uD83D\\uDE80 literal \\n"}}]
}"#;
const ESCAPED: &str = r#"{
  "name": "unicode",
  "description": "before \uD83D\uDE80\uD83E\uDDEA after\n\"quoted\" C:\\tmp\\uD83D\\uDE80 literal \\n",
  "context": {"text": "before \uD83D\uDE80\uD83E\uDDEA after\n\"quoted\" C:\\tmp\\uD83D\\uDE80 literal \\n"},
  "steps": [{"id": "ask", "prompt": "before \uD83D\uDE80\uD83E\uDDEA after\n\"quoted\" C:\\tmp\\uD83D\\uDE80 literal \\n",
    "context": {"text": "before \uD83D\uDE80\uD83E\uDDEA after\n\"quoted\" C:\\tmp\\uD83D\\uDE80 literal \\n"}}]
}"#;
const TEXT: &str = "before 🚀🧪 after\n\"quoted\" C:\\tmp\\uD83D\\uDE80 literal \\n";

fn parser() -> RecipeParser {
    RecipeParser::new().without_field_warnings()
}

fn assert_text(recipe: &Recipe) {
    assert_eq!(recipe.description, TEXT);
    assert_eq!(recipe.steps[0].prompt.as_deref(), Some(TEXT));
    assert_eq!(recipe.context["text"], TEXT);
    assert_eq!(recipe.steps[0].sub_context.as_ref().unwrap()["text"], TEXT);
    assert_eq!(recipe.steps[0].effective_type(), StepType::Agent);
}

fn document(fields: &str, steps: &str) -> String {
    format!(r#"{{"name":"unicode","description":"\uD83D\uDE80",{fields}"steps":{steps}}}"#)
}

#[test]
fn raw_utf8_control_preserves_complete_text() {
    assert_text(&parse_recipe(RAW).expect("the independent UTF-8 control must parse"));
}

#[test]
fn escaped_surrogate_pairs_match_raw_utf8_exactly() {
    let raw = parse_recipe(RAW).unwrap();
    let escaped = parse_recipe(ESCAPED).expect("valid JSON surrogate pairs must parse");
    assert_text(&escaped);
    assert_eq!(
        serde_json::to_value(escaped).unwrap(),
        serde_json::to_value(raw).unwrap()
    );
}

#[test]
fn parse_file_accepts_both_encodings() {
    let dir = tempfile::tempdir().unwrap();
    for (name, input) in [("raw.json", RAW), ("escaped.json", ESCAPED)] {
        let path = dir.path().join(name);
        std::fs::write(&path, input).unwrap();
        assert_text(
            &parser()
                .parse_file(&path)
                .expect("JSON files must preserve text"),
        );
    }
}

#[test]
fn malformed_surrogates_are_rejected_everywhere() {
    let invalid = [
        r"\uD800",
        r"\uDC00",
        r"\uDE80\uD83D",
        r"\uD83D\u0041",
        r"\uD83Dtext",
        r"\uD83D\uDE8",
        r"\uD83D\uZZZZ",
        r"\uD83D\q",
    ];
    for bad in invalid {
        let locations = [
            format!(r#"{{"name":"bad","description":"{bad}","steps":[{{"id":"s"}}]}}"#),
            document(
                &format!(r#""author":"{bad}","#),
                r#"[{"id":"s","command":"true"}]"#,
            ),
            document("", &format!(r#"[{{"id":"s","prompt":"{bad}"}}]"#)),
            document(
                &format!(r#""context":{{"x":[{{"text":"{bad}"}}]}},"#),
                r#"[{"id":"s"}]"#,
            ),
            document("", &format!(r#"[{{"id":"s","context":{{"x":"{bad}"}}}}]"#)),
            document(
                &format!(r#""ignored":{{"nested":["{bad}"]}},"#),
                r#"[{"id":"s"}]"#,
            ),
            document(
                "",
                &format!(r#"[{{"id":"s","ignored":{{"nested":["{bad}"]}}}}]"#),
            ),
            document(&format!(r#""{bad}":null,"#), r#"[{"id":"s"}]"#),
        ];
        for (location, input) in locations.iter().enumerate() {
            assert!(
                parser().parse(input).is_err(),
                "accepted malformed escape {bad:?} at location {location}"
            );
        }
    }
}

#[test]
fn json_fallback_requires_one_complete_document_and_bounded_nesting() {
    let valid = document("", r#"[{"id":"s","command":"true"}]"#);
    for suffix in [" {}", " null", " trailing", "\n---\nname: other"] {
        assert!(parser().parse(&format!("{valid}{suffix}")).is_err());
    }
    let nested = format!("{}0{}", "[".repeat(256), "]".repeat(256));
    let deep = document(&format!(r#""ignored":{nested},"#), r#"[{"id":"s"}]"#);
    assert!(
        parser().parse(&deep).is_err(),
        "ignored values must retain recursion protection"
    );
    assert!(
        parser().parse(&valid).is_ok(),
        "the complete document is valid"
    );
}

#[test]
fn recognised_duplicate_fields_and_step_ids_remain_errors() {
    let duplicates = [
        document(r#""name":"again","#, r#"[{"id":"s"}]"#),
        document(r#""context":{},"context":{},"#, r#"[{"id":"s"}]"#),
        document("", r#"[{"id":"s","id":"again"}]"#),
        document("", r#"[{"id":"s","prompt":"a","prompt":"b"}]"#),
        document("", r#"[{"id":"s","context":{},"context":{}}]"#),
        document(
            "",
            r#"[{"id":"s","nonfatal":true,"continue_on_error":false}]"#,
        ),
        document(
            r#""hooks":{"pre_step":"a","pre_step":"b"},"#,
            r#"[{"id":"s"}]"#,
        ),
        document("", r#"[{"id":"s"},{"id":"s"}]"#),
    ];
    for (case, input) in duplicates.iter().enumerate() {
        assert!(
            parser().parse(input).is_err(),
            "duplicate case {case} must fail"
        );
        let raw = input.replace(r"\uD83D\uDE80", "🚀");
        assert!(
            parser().parse(&raw).is_err(),
            "raw duplicate case {case} must fail"
        );
    }
}

#[test]
fn map_and_unknown_duplicates_keep_the_raw_utf8_baseline() {
    let cases = [
        ("context", r#""context":{"x":1,"x":2},"#, r#"[{"id":"s"}]"#),
        (
            "nested context",
            r#""context":{"x":{"y":1,"y":2}},"#,
            r#"[{"id":"s"}]"#,
        ),
        (
            "step context",
            "",
            r#"[{"id":"s","context":{"x":1,"x":2}}]"#,
        ),
        (
            "unknown top-level",
            r#""ignored":1,"ignored":2,"#,
            r#"[{"id":"s"}]"#,
        ),
        (
            "unknown nested",
            r#""ignored":{"x":1,"x":2},"#,
            r#"[{"id":"s"}]"#,
        ),
        (
            "unknown step",
            "",
            r#"[{"id":"s","ignored":1,"ignored":2}]"#,
        ),
    ];
    let mut mismatches = Vec::new();
    for (name, fields, steps) in cases {
        let escaped = document(fields, steps);
        let raw = escaped.replace(r"\uD83D\uDE80", "🚀");
        let baseline = parser().parse(&raw);
        let candidate = parser().parse(&escaped);
        println!(
            "Duplicate baseline {name}: raw accepted={}, escaped accepted={}",
            baseline.is_ok(),
            candidate.is_ok()
        );
        if baseline.is_ok() != candidate.is_ok() {
            mismatches.push(name);
        }
        if let (Ok(raw), Ok(escaped)) = (baseline, candidate) {
            assert_eq!(
                serde_json::to_value(raw).unwrap(),
                serde_json::to_value(escaped).unwrap(),
                "{name}"
            );
        }
    }
    assert!(
        mismatches.is_empty(),
        "duplicate acceptance differs: {mismatches:?}"
    );
}

#[test]
fn schema_types_and_required_fields_remain_checked() {
    let invalid = [
        r#"{"description":"\uD83D\uDE80","steps":[{"id":"s"}]}"#.to_string(),
        r#"{"name":[],"description":"\uD83D\uDE80","steps":[{"id":"s"}]}"#.to_string(),
        r#"{"name":"","description":"\uD83D\uDE80","steps":[{"id":"s"}]}"#.to_string(),
        document("", "[]"),
        document("", r#"[{"command":"true"}]"#),
        document("", r#"[{"id":""}]"#),
        document("", r#"[{"id":"s","prompt":[]}]"#),
        document("", r#"[{"id":"s","type":"other"}]"#),
        document("", r#"[{"id":"s","timeout":-1}]"#),
        document(r#""context":[],"#, r#"[{"id":"s"}]"#),
    ];
    for (case, input) in invalid.iter().enumerate() {
        assert!(
            parser().parse(input).is_err(),
            "invalid schema case {case} must fail"
        );
        assert!(
            parser()
                .parse(&input.replace(r"\uD83D\uDE80", "🚀"))
                .is_err()
        );
    }
    let valid = document("", r#"[{"id":"s","command":"true"}]"#);
    assert!(parser().parse(&valid).is_ok(), "valid schema must pass");
}

#[test]
fn defaults_aliases_inference_and_references_are_preserved() {
    let input = document(
        "",
        r#"[
      {"id":"prompt","prompt":"hello"},
      {"id":"agent","agent":"fake","command":"unused"},
      {"id":"bash","command":"true","fatal":false},
      {"id":"child","recipe":"child","nonfatal":true},
      {"id":"explicit","type":"bash","prompt":"unused","command":"true"}
    ]"#,
    );
    let recipe = parser().parse(&input).unwrap();
    assert_eq!(recipe.version, "1.0.0");
    assert!(recipe.context.is_empty());
    let types: Vec<_> = recipe.steps.iter().map(|s| s.effective_type()).collect();
    assert_eq!(
        types,
        [
            StepType::Agent,
            StepType::Agent,
            StepType::Bash,
            StepType::Recipe,
            StepType::Bash
        ]
    );
    assert!(recipe.steps[2].is_nonfatal());
    assert!(recipe.steps[3].is_nonfatal());
    assert!(!recipe.steps[0].parse_json);
    for reference in ["../child", "child.yaml", "/tmp/child"] {
        let invalid = document("", &format!(r#"[{{"id":"s","recipe":"{reference}"}}]"#));
        let error = parser().parse(&invalid).unwrap_err();
        assert!(
            matches!(error, ParseError::Invalid(_)),
            "reference validation must follow decoding: {error}"
        );
    }
}

#[test]
fn yaml_comments_aliases_quoting_and_block_text_are_preserved() {
    let yaml = r#"
# A YAML document must retain YAML semantics.
name: yaml
description: 'literal \uD83D\uDE80'
context:
  text: &text 'C:\tmp\n'
  copy: *text
steps:
  - id: ask
    prompt: |
      first 🚀
      second "quoted"
    context:
      text: *text
"#;
    let recipe = parser().parse(yaml).unwrap();
    assert_eq!(recipe.description, r"literal \uD83D\uDE80");
    assert_eq!(recipe.context["text"], r"C:\tmp\n");
    assert_eq!(recipe.context["copy"], recipe.context["text"]);
    assert_eq!(
        recipe.steps[0].prompt.as_deref(),
        Some("first 🚀\nsecond \"quoted\"\n")
    );
    assert_eq!(
        recipe.steps[0].sub_context.as_ref().unwrap()["text"],
        recipe.context["text"]
    );
}

#[test]
fn original_source_byte_limits_apply_to_strings_and_files() {
    const LIMIT: usize = 1_000_000;
    let base = document("", r#"[{"id":"s","command":"true"}]"#);
    let exact = format!("{base}{}", " ".repeat(LIMIT - base.len()));
    assert_eq!(exact.len(), LIMIT);
    let over = format!("{exact} ");
    let many_escapes = document(
        &format!(r#""author":"{}","#, r"\uD83D\uDE80".repeat(84_000)),
        r#"[{"id":"s"}]"#,
    );
    assert!(many_escapes.len() > LIMIT);
    assert!(
        serde_json::from_str::<Value>(&many_escapes).unwrap()["author"]
            .as_str()
            .unwrap()
            .len()
            < LIMIT
    );
    let dir = tempfile::tempdir().unwrap();
    for input in [&over, &many_escapes] {
        let check = |error| match error {
            ParseError::FileTooLarge { size, max } => {
                assert_eq!(size, input.len());
                assert_eq!(max, LIMIT);
            }
            other => panic!("source-byte limit must run before decoding: {other}"),
        };
        check(parser().parse(input).unwrap_err());
        let path = dir.path().join("oversized.json");
        std::fs::write(&path, input).unwrap();
        check(parser().parse_file(&path).unwrap_err());
    }
    let raw_base = base.replace(r"\uD83D\uDE80", "🚀");
    let raw_exact = format!("{raw_base}{}", " ".repeat(LIMIT - raw_base.len()));
    assert!(
        parser().parse(&raw_exact).is_ok(),
        "the raw boundary control must parse"
    );
    assert!(
        parser().parse(&exact).is_ok(),
        "the exact escaped boundary must parse"
    );
}

fn warning_document() -> String {
    document(
        r#""top_typo":true,"#,
        r#"[{"id":"warn\n\u001b","command":"true","step_typo":1,"max_env_value_bytes":42,"bad\n\u001b":true}]"#,
    )
}

#[test]
fn public_validation_preserves_unknown_and_retired_warnings() {
    let escaped = warning_document();
    for input in [escaped.replace(r"\uD83D\uDE80", "🚀"), escaped] {
        let warnings = validate_recipe(&input).unwrap();
        assert_eq!(warnings.len(), 4, "{warnings:?}");
        for field in [
            "top_typo",
            "step_typo",
            "max_env_value_bytes",
            r"bad\n\u{1b}",
        ] {
            assert_eq!(
                warnings.iter().filter(|w| w.contains(field)).count(),
                1,
                "{warnings:?}"
            );
        }
        assert!(warnings.iter().any(|w| w.contains("is retired")));
        assert!(warnings.iter().all(|w| !w.contains(['\n', '\u{1b}'])));
    }
}

#[test]
fn warning_limits_and_identifier_truncation_survive_json_decoding() {
    let mut step = serde_json::Map::new();
    step.insert("id".into(), json!("s"));
    step.insert("command".into(), json!("true"));
    for n in 0..105 {
        step.insert(format!("unknown_{n}"), json!(true));
    }
    let input = document("", &serde_json::to_string(&[step]).unwrap());
    let warnings = validate_recipe(&input).unwrap();
    assert_eq!(warnings.len(), 101);
    assert_eq!(warnings.last().unwrap(), "… and 5 more unknown step fields");
    let long = "x".repeat(200);
    let input = document(
        "",
        &format!(r#"[{{"id":"s","command":"true","{long}":1}}]"#),
    );
    let warnings = validate_recipe(&input).unwrap();
    assert!(warnings[0].contains(&format!("{}…", "x".repeat(128))));
    assert!(!warnings[0].contains(&"x".repeat(129)));
}

fn cli(dir: &Path, input: &str, mode: &str) -> (i32, String, String) {
    let home = dir.join("home");
    let tmp = dir.join("tmp");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::create_dir_all(dir.join(".git")).unwrap();
    let path = dir.join("recipe.json");
    std::fs::write(&path, input).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_recipe-runner-rs"))
        .arg(path)
        .arg(mode)
        .current_dir(dir)
        .env_clear()
        .env("HOME", home)
        .env("TMPDIR", &tmp)
        .env("TMP", &tmp)
        .env("TEMP", &tmp)
        .env("PATH", "/usr/bin:/bin")
        .env("RUST_TEST_THREADS", "2")
        .output()
        .expect("the compiled CLI must execute");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

#[test]
fn cli_validate_only_reports_warnings_once_on_stdout() {
    let escaped = warning_document();
    for input in [escaped.replace(r"\uD83D\uDE80", "🚀"), escaped] {
        let dir = tempfile::tempdir().unwrap();
        let (code, stdout, stderr) = cli(dir.path(), &input, "--validate-only");
        assert_eq!(code, 0, "{stderr}");
        for field in [
            "top_typo",
            "step_typo",
            "max_env_value_bytes",
            r"bad\n\u{1b}",
        ] {
            assert_eq!(stdout.matches(field).count(), 1, "{stdout}");
            assert!(!stderr.contains(field), "{stderr}");
        }
        assert!(!stdout.contains('\u{1b}'));
    }
}

#[test]
fn cli_dry_run_keeps_parse_time_warnings_on_stderr() {
    let escaped = warning_document();
    for input in [escaped.replace(r"\uD83D\uDE80", "🚀"), escaped] {
        let dir = tempfile::tempdir().unwrap();
        let (code, _stdout, stderr) = cli(dir.path(), &input, "--dry-run");
        assert_eq!(code, 0, "{stderr}");
        for field in ["step_typo", "max_env_value_bytes", r"bad\n\u{1b}"] {
            assert_eq!(stderr.matches(field).count(), 1, "{stderr}");
        }
        assert!(stderr.contains(r"step 'warn\n\u{1b}'"), "{stderr}");
        assert!(!stderr.contains('\u{1b}'));
    }
}

#[test]
fn existing_deserialisers_characterise_fallback_guards() {
    // A value check must validate ignored content, while Recipe stays typed.
    assert!(serde_json::from_str::<Recipe>(ESCAPED).is_ok());
    assert!(serde_json::from_str::<serde_yaml::Value>(ESCAPED).is_ok());
    let malformed = r#"{"name":"guard","steps":[{"id":"s"}],"ignored":{"x":"\uD800"}}"#;
    println!(
        "Typed JSON accepts malformed ignored value: {}",
        serde_json::from_str::<Recipe>(malformed).is_ok()
    );
    assert!(serde_json::from_str::<serde_yaml::Value>(malformed).is_err());
    assert!(serde_json::from_str::<Value>(malformed).is_err());
    for duplicate in [r#"{"x":1,"x":2}"#, r#"{"nested":{"x":1,"x":2}}"#] {
        println!(
            "Duplicate value guards: YAML accepts={}, JSON-to-YAML accepts={}",
            serde_yaml::from_str::<serde_yaml::Value>(duplicate).is_ok(),
            serde_json::from_str::<serde_yaml::Value>(duplicate).is_ok()
        );
        assert!(serde_json::from_str::<Value>(duplicate).is_ok());
    }
}
