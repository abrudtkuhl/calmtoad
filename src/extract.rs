//! Named CSS extraction rules over the same HTML/DOM snapshot used by the content audits.
//!
//! This is deliberately not a browser automation language: selectors are compiled once per run,
//! values are bounded, and outcomes distinguish no match from a missing attribute, an
//! unavailable rendered document, a partial capture, and a limit.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use scraper::{ElementRef, Html, Node, Selector};
use serde::{Deserialize, Serialize};

use crate::indexability::EvidenceStage;

pub const MAX_RULES: usize = 20;
pub const MAX_NAME_CHARS: usize = 80;
pub const MAX_SELECTOR_CHARS: usize = 2_048;
pub const MAX_ATTRIBUTE_CHARS: usize = 128;
pub const MAX_VALUES_PER_RULE: usize = 100;
pub const MAX_VALUE_CHARS: usize = 4_096;
pub const MAX_PAGE_VALUE_BYTES: usize = 64 * 1024;
pub const MAX_RUN_VALUE_BYTES: usize = 32 * 1024 * 1024;
const CONFIG_FILE: &str = "extraction.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractionRule {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub selector: String,
    pub output: ExtractionOutput,
    #[serde(default)]
    pub attribute: String,
    pub mode: MatchMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionOutput {
    Text,
    Attribute,
}

impl ExtractionOutput {
    pub fn label(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Attribute => "Attribute",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    First,
    All,
}

impl MatchMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::First => "First",
            Self::All => "All",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtractionOutcome {
    pub rule_id: String,
    pub state: ExtractionState,
    pub values: Vec<String>,
    pub stage: Option<EvidenceStage>,
    pub reason: String,
    pub limited: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ExtractionState {
    #[default]
    NoMatch,
    Matched,
    AttributeMissing,
    NotApplicable,
    Unavailable,
    Partial,
    Limited,
    CapReached,
    Invalid,
}

impl ExtractionState {
    pub fn label(self) -> &'static str {
        match self {
            Self::NoMatch => "No match",
            Self::Matched => "Matched",
            Self::AttributeMissing => "Attribute missing",
            Self::NotApplicable => "Not applicable",
            Self::Unavailable => "Unavailable",
            Self::Partial => "Partial",
            Self::Limited => "Limited",
            Self::CapReached => "Cap reached",
            Self::Invalid => "Invalid",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleError {
    pub index: usize,
    pub field: &'static str,
    pub message: String,
}

pub struct CompiledRule {
    pub rule: ExtractionRule,
    selector: Selector,
}

pub struct CompiledRules {
    pub rules: Vec<CompiledRule>,
}

impl CompiledRules {
    pub fn compile(rules: &[ExtractionRule]) -> Result<Self, Vec<RuleError>> {
        let mut errors = Vec::new();
        if rules.len() > MAX_RULES {
            errors.push(RuleError {
                index: MAX_RULES,
                field: "rules",
                message: format!("At most {MAX_RULES} rules are allowed"),
            });
        }
        let mut names = HashSet::new();
        let mut ids = HashSet::new();
        let mut reserved = crate::inventory::HEADERS
            .iter()
            .map(|header| header.to_ascii_lowercase())
            .collect::<HashSet<_>>();
        for header in ["EXTRACT", "RUN", "URL"] {
            reserved.insert(header.to_ascii_lowercase());
        }
        let mut compiled = Vec::new();
        for (index, rule) in rules.iter().enumerate() {
            if rule.name.trim().is_empty() {
                errors.push(RuleError {
                    index,
                    field: "name",
                    message: "Name is required".into(),
                });
            } else if rule.name.chars().count() > MAX_NAME_CHARS {
                errors.push(RuleError {
                    index,
                    field: "name",
                    message: format!("Name is longer than {MAX_NAME_CHARS} characters"),
                });
            } else if !names.insert(rule.name.trim().to_ascii_lowercase()) {
                errors.push(RuleError {
                    index,
                    field: "name",
                    message: "Names must be unique".into(),
                });
            } else if reserved.contains(&rule.name.trim().to_ascii_lowercase()) {
                errors.push(RuleError {
                    index,
                    field: "name",
                    message: "Name collides with a reserved inventory column".into(),
                });
            }
            if rule.id.trim().is_empty() {
                errors.push(RuleError {
                    index,
                    field: "id",
                    message: "Rule ID is required".into(),
                });
            } else if !ids.insert(rule.id.clone()) {
                errors.push(RuleError {
                    index,
                    field: "id",
                    message: "Rule IDs must be unique".into(),
                });
            }
            if rule.selector.trim().is_empty() {
                errors.push(RuleError {
                    index,
                    field: "selector",
                    message: "Selector is required".into(),
                });
            } else if rule.selector.chars().count() > MAX_SELECTOR_CHARS {
                errors.push(RuleError {
                    index,
                    field: "selector",
                    message: format!("Selector is longer than {MAX_SELECTOR_CHARS} characters"),
                });
            }
            if rule.output == ExtractionOutput::Attribute {
                if rule.attribute.trim().is_empty() {
                    errors.push(RuleError {
                        index,
                        field: "attribute",
                        message: "Attribute name is required for Attribute output".into(),
                    });
                } else if rule.attribute.chars().count() > MAX_ATTRIBUTE_CHARS {
                    errors.push(RuleError {
                        index,
                        field: "attribute",
                        message: format!(
                            "Attribute name is longer than {MAX_ATTRIBUTE_CHARS} characters"
                        ),
                    });
                }
            }
            match Selector::parse(&rule.selector) {
                Ok(selector) => compiled.push(CompiledRule {
                    rule: rule.clone(),
                    selector,
                }),
                Err(_) => errors.push(RuleError {
                    index,
                    field: "selector",
                    message: "Selector could not be parsed".into(),
                }),
            }
        }
        if errors.is_empty() {
            Ok(Self { rules: compiled })
        } else {
            Err(errors)
        }
    }
}

pub fn evaluate(
    compiled: &CompiledRules,
    html: Option<&str>,
    stage: EvidenceStage,
    applicable: bool,
    partial: bool,
    run_budget: &mut usize,
) -> Vec<ExtractionOutcome> {
    if !applicable {
        return compiled
            .rules
            .iter()
            .map(|rule| ExtractionOutcome {
                rule_id: rule.rule.id.clone(),
                state: ExtractionState::NotApplicable,
                reason: "Not an HTML response".into(),
                ..ExtractionOutcome::default()
            })
            .collect();
    }
    let Some(html) = html else {
        return compiled
            .rules
            .iter()
            .map(|rule| ExtractionOutcome {
                rule_id: rule.rule.id.clone(),
                state: ExtractionState::Unavailable,
                stage: Some(stage),
                reason: "The selected capture stage has no HTML document".into(),
                ..ExtractionOutcome::default()
            })
            .collect();
    };
    let doc = Html::parse_document(html);
    let mut page_budget = MAX_PAGE_VALUE_BYTES;
    let mut outcomes = Vec::with_capacity(compiled.rules.len());
    for rule in &compiled.rules {
        if *run_budget == 0 {
            outcomes.push(ExtractionOutcome {
                rule_id: rule.rule.id.clone(),
                state: ExtractionState::CapReached,
                stage: Some(stage),
                reason: "The run extraction value cap was reached".into(),
                limited: true,
                ..ExtractionOutcome::default()
            });
            continue;
        }
        let mut outcome = ExtractionOutcome {
            rule_id: rule.rule.id.clone(),
            stage: Some(stage),
            ..ExtractionOutcome::default()
        };
        let mut values = Vec::new();
        let mut limited = false;
        let mut limit_reason: Option<String> = None;
        let mut missing = 0_usize;
        for element in doc.select(&rule.selector) {
            if values.len() >= MAX_VALUES_PER_RULE {
                limited = true;
                limit_reason = Some(format!(
                    "At most {MAX_VALUES_PER_RULE} values are retained per rule and page"
                ));
                break;
            }
            let (raw_value, produced_truncated) = match rule.rule.output {
                ExtractionOutput::Text => element_text_bounded(element, MAX_VALUE_CHARS),
                ExtractionOutput::Attribute => match element.attr(&rule.rule.attribute) {
                    Some(value) => truncate_chars_flagged(value, MAX_VALUE_CHARS),
                    None => {
                        missing += 1;
                        if rule.rule.mode == MatchMode::First {
                            break;
                        }
                        continue;
                    }
                },
            };
            if produced_truncated {
                limited = true;
                limit_reason = Some(format!("Value truncated at {MAX_VALUE_CHARS} characters"));
            }
            let (mut value, budget_truncated) =
                truncate_bytes_flagged(&raw_value, page_budget.min(*run_budget));
            if budget_truncated {
                limited = true;
                limit_reason = Some("The extraction value budget was reached".into());
                if value.is_empty() {
                    break;
                }
            }
            // An empty extracted value is a real observation when the budgets still have room.
            if value.is_empty() && (page_budget == 0 || *run_budget == 0) {
                limited = true;
                limit_reason = Some("The extraction value budget was reached".into());
                break;
            }
            page_budget -= value.len();
            *run_budget -= value.len();
            if produced_truncated {
                // Keep the ellipsis produced by the bounded text/value helper.
                value.shrink_to_fit();
            }
            values.push(value);
            if rule.rule.mode == MatchMode::First {
                break;
            }
            if page_budget == 0 || *run_budget == 0 {
                limited = true;
                limit_reason = Some("The extraction value budget was reached".into());
                break;
            }
        }
        let missing_note = (missing > 0).then(|| {
            format!(
                "{missing} matched element{} lacked the {} attribute",
                if missing == 1 { "" } else { "s" },
                rule.rule.attribute
            )
        });
        outcome.values = values;
        if partial {
            outcome.state = ExtractionState::Partial;
            outcome.reason =
                "The capture is partial; matches and no-match are not completed evidence".into();
        } else if limited {
            outcome.state = ExtractionState::Limited;
            outcome.reason = match (limit_reason, missing_note) {
                (Some(limit), Some(missing)) => format!("{limit}; {missing}"),
                (Some(limit), None) => limit,
                (None, Some(missing)) => missing,
                (None, None) => "The extraction hit a value limit".into(),
            };
        } else if let Some(missing) = missing_note {
            outcome.state = ExtractionState::AttributeMissing;
            outcome.reason = missing;
        } else if outcome.values.is_empty() {
            outcome.state = ExtractionState::NoMatch;
        } else {
            outcome.state = ExtractionState::Matched;
        }
        if limited {
            outcome.limited = true;
        }
        outcomes.push(outcome);
    }
    outcomes
}

/// Produce normalized descendant text without first collecting an unbounded string. Stops as
/// soon as the value cap is reached and reports that the value was truncated.
fn element_text_bounded(element: ElementRef<'_>, max_chars: usize) -> (String, bool) {
    let max_chars = max_chars.max(1);
    let mut text = String::new();
    let mut chars = 0_usize;
    for node in element.descendants() {
        let Node::Text(value) = node.value() else {
            continue;
        };
        let excluded = node.ancestors().skip(1).any(|ancestor| {
            ancestor
                .value()
                .as_element()
                .is_some_and(|element| matches!(element.name(), "script" | "style" | "template"))
        });
        if excluded {
            continue;
        }
        for word in value.text.split_whitespace() {
            let separator = usize::from(!text.is_empty());
            let word_chars = word.chars().count();
            if chars + separator + word_chars > max_chars {
                if separator == 1 && chars + 1 < max_chars {
                    text.push(' ');
                    chars += 1;
                }
                for ch in word.chars() {
                    if chars + 2 > max_chars {
                        break;
                    }
                    text.push(ch);
                    chars += 1;
                }
                while text.chars().count() + 1 > max_chars {
                    text.pop();
                }
                text.push('…');
                return (text, true);
            }
            if separator == 1 {
                text.push(' ');
                chars += 1;
            }
            text.push_str(word);
            chars += word_chars;
        }
    }
    (text, false)
}

/// Bound a value by characters, marking whether an ellipsis replaced content.
fn truncate_chars_flagged(value: &str, max: usize) -> (String, bool) {
    if value.chars().count() <= max {
        return (value.to_string(), false);
    }
    let mut out: String = value.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    (out, true)
}

/// Bound a value by bytes without splitting a code point, marking whether content was removed.
fn truncate_bytes_flagged(value: &str, max: usize) -> (String, bool) {
    if value.len() <= max {
        return (value.to_string(), false);
    }
    if max == 0 {
        return (String::new(), true);
    }
    let ellipsis = '…';
    let mut end = max.saturating_sub(ellipsis.len_utf8());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = value[..end].to_string();
    out.push(ellipsis);
    if out.len() > max {
        return (String::new(), true);
    }
    (out, true)
}

fn first_invalid_persisted_id(rules: &[ExtractionRule]) -> Option<String> {
    let mut seen = HashSet::new();
    for rule in rules {
        if rule.id.trim().is_empty() {
            return Some("empty rule ID".into());
        }
        if !seen.insert(rule.id.clone()) {
            return Some(format!("duplicate rule ID {}", rule.id));
        }
    }
    None
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Config {
    #[serde(default = "default_schema")]
    schema: u32,
    #[serde(default)]
    rules: Vec<ExtractionRule>,
}

fn default_schema() -> u32 {
    1
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema: default_schema(),
            rules: Vec::new(),
        }
    }
}

fn path_in(dir: &Path) -> PathBuf {
    dir.join(CONFIG_FILE)
}

pub fn load_from(dir: &Path) -> (Vec<ExtractionRule>, Option<String>) {
    let Ok(bytes) = fs::read(path_in(dir)) else {
        return (Vec::new(), None);
    };
    match serde_json::from_slice::<Config>(&bytes) {
        Ok(config) => {
            if let Some(problem) = first_invalid_persisted_id(&config.rules) {
                return (
                    Vec::new(),
                    Some(format!(
                        "Custom extraction rules could not be used: {problem}. Fix or remove the saved extraction.json."
                    )),
                );
            }
            (config.rules, None)
        }
        Err(error) => (
            Vec::new(),
            Some(format!(
                "Custom extraction rules could not be read: {error}"
            )),
        ),
    }
}

pub fn load() -> (Vec<ExtractionRule>, Option<String>) {
    load_from(&crate::update::app_dir())
}

pub fn save_to(dir: &Path, rules: &[ExtractionRule]) -> Result<(), String> {
    if let Some(problem) = first_invalid_persisted_id(rules) {
        return Err(format!("Custom extraction rules were not saved: {problem}"));
    }
    fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let config = Config {
        schema: default_schema(),
        rules: rules.to_vec(),
    };
    let bytes = serde_json::to_vec_pretty(&config).map_err(|error| error.to_string())?;
    fs::write(path_in(dir), bytes).map_err(|error| error.to_string())
}

pub fn save(rules: &[ExtractionRule]) -> Result<(), String> {
    save_to(&crate::update::app_dir(), rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(
        name: &str,
        selector: &str,
        output: ExtractionOutput,
        mode: MatchMode,
    ) -> ExtractionRule {
        ExtractionRule {
            id: name.to_ascii_lowercase(),
            name: name.into(),
            selector: selector.into(),
            output,
            attribute: if output == ExtractionOutput::Attribute {
                "href".into()
            } else {
                String::new()
            },
            mode,
        }
    }

    #[test]
    fn validates_names_selectors_attributes_and_limits() {
        let rules = vec![
            rule("Title", "title", ExtractionOutput::Text, MatchMode::First),
            rule("title", "h1", ExtractionOutput::Text, MatchMode::All),
            ExtractionRule {
                selector: "a[".into(),
                ..rule("Bad", "a[", ExtractionOutput::Attribute, MatchMode::All)
            },
        ];
        let errors = CompiledRules::compile(&rules).err().expect("errors");
        assert!(errors.iter().any(|error| error.field == "name"));
        assert!(errors.iter().any(|error| error.field == "selector"));
    }

    #[test]
    fn evaluates_text_attribute_first_all_and_missing_attribute() {
        let html = r#"<html><body><h1>Hello <b>nested</b></h1><a href="/one">One</a><a href="/two">Two</a><a>No href</a><script>ignore</script></body></html>"#;
        let rules = vec![
            rule("Heading", "h1", ExtractionOutput::Text, MatchMode::First),
            rule("Links", "a", ExtractionOutput::Attribute, MatchMode::All),
            rule(
                "Missing",
                "a",
                ExtractionOutput::Attribute,
                MatchMode::First,
            ),
        ];
        let compiled = CompiledRules::compile(&rules).expect("compiled");
        let mut budget = MAX_RUN_VALUE_BYTES;
        let outcomes = evaluate(
            &compiled,
            Some(html),
            EvidenceStage::Document,
            true,
            false,
            &mut budget,
        );
        assert_eq!(outcomes[0].state, ExtractionState::Matched);
        assert_eq!(outcomes[0].values, vec!["Hello nested"]);
        assert_eq!(outcomes[1].values, vec!["/one", "/two"]);
        assert_eq!(outcomes[2].state, ExtractionState::Matched);
    }

    #[test]
    fn distinguishes_empty_values_no_match_partial_and_unavailable() {
        let html = r#"<p data-x=""></p><div>text</div>"#;
        let rules = vec![
            ExtractionRule {
                id: "attr".into(),
                name: "Attribute".into(),
                selector: "p".into(),
                output: ExtractionOutput::Attribute,
                attribute: "data-x".into(),
                mode: MatchMode::First,
            },
            rule(
                "Missing",
                "section",
                ExtractionOutput::Text,
                MatchMode::First,
            ),
        ];
        let compiled = CompiledRules::compile(&rules).expect("compiled");
        let mut budget = MAX_RUN_VALUE_BYTES;
        let outcomes = evaluate(
            &compiled,
            Some(html),
            EvidenceStage::Document,
            true,
            true,
            &mut budget,
        );
        assert_eq!(outcomes[0].values, vec![""]);
        assert_eq!(outcomes[0].state, ExtractionState::Partial);
        assert_eq!(outcomes[1].state, ExtractionState::Partial);

        let unavailable = evaluate(
            &compiled,
            None,
            EvidenceStage::Rendered,
            true,
            false,
            &mut budget,
        );
        assert_eq!(unavailable[0].state, ExtractionState::Unavailable);
    }

    #[test]
    fn duplicate_rule_ids_are_rejected_before_capture_and_from_saved_state() {
        let rules = vec![
            ExtractionRule {
                id: "same".into(),
                name: "First".into(),
                selector: "h1".into(),
                output: ExtractionOutput::Text,
                attribute: String::new(),
                mode: MatchMode::First,
            },
            ExtractionRule {
                id: "same".into(),
                name: "Second".into(),
                selector: "h2".into(),
                output: ExtractionOutput::Text,
                attribute: String::new(),
                mode: MatchMode::First,
            },
        ];
        let errors = CompiledRules::compile(&rules).err().expect("errors");
        assert!(errors.iter().any(|error| error.field == "id"));

        let dir = tempfile::TempDir::new().expect("dir");
        let config = serde_json::json!({
            "schema": 1,
            "rules": rules,
        });
        std::fs::write(dir.path().join(CONFIG_FILE), config.to_string()).expect("write");
        let (loaded, error) = load_from(dir.path());
        assert!(loaded.is_empty(), "malformed persisted IDs are not loaded");
        assert!(error.unwrap_or_default().contains("duplicate rule ID"));
        assert!(save_to(dir.path(), &rules).is_err());
    }

    #[test]
    fn saved_rules_round_trip_with_stable_ids() {
        let dir = tempfile::TempDir::new().expect("dir");
        let rules = vec![rule(
            "Price",
            ".price",
            ExtractionOutput::Text,
            MatchMode::First,
        )];
        save_to(dir.path(), &rules).expect("save");
        let (loaded, error) = load_from(dir.path());
        assert!(error.is_none());
        assert_eq!(loaded, rules);
        assert_eq!(loaded[0].id, "price");
    }

    #[test]
    fn value_caps_and_run_caps_are_explicit() {
        let html = (0..MAX_VALUES_PER_RULE + 2)
            .map(|index| format!("<p>{index}</p>"))
            .collect::<String>();
        let rules = vec![rule(
            "Paragraphs",
            "p",
            ExtractionOutput::Text,
            MatchMode::All,
        )];
        let compiled = CompiledRules::compile(&rules).expect("compiled");
        let mut budget = MAX_RUN_VALUE_BYTES;
        let outcomes = evaluate(
            &compiled,
            Some(&html),
            EvidenceStage::Document,
            true,
            false,
            &mut budget,
        );
        assert_eq!(outcomes[0].values.len(), MAX_VALUES_PER_RULE);
        assert_eq!(outcomes[0].state, ExtractionState::Limited);
        assert!(outcomes[0].limited);

        let mut exhausted = 0;
        let capped = evaluate(
            &compiled,
            Some(&html),
            EvidenceStage::Document,
            true,
            false,
            &mut exhausted,
        );
        assert_eq!(capped[0].state, ExtractionState::CapReached);
    }

    #[test]
    fn malformed_saved_configuration_is_recoverable() {
        let dir = tempfile::TempDir::new().expect("dir");
        std::fs::write(dir.path().join(CONFIG_FILE), b"not json").expect("write");
        let (rules, error) = load_from(dir.path());
        assert!(rules.is_empty());
        assert!(error.is_some());
    }
}
