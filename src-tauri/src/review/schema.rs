//! The JSON shape the reviewer must return (passed to `claude --json-schema`).

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct RawReview {
    pub verdict: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<RawFinding>,
    #[serde(default)]
    pub critical_files: Vec<RawCriticalFile>,
}

/// A changed file the reviewer recommends the user reads personally.
#[derive(Debug, Clone, Deserialize)]
pub struct RawCriticalFile {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub focus_lines: Vec<RawLineRange>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawLineRange {
    #[serde(default)]
    pub start: i64,
    #[serde(default)]
    pub end: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawFinding {
    pub severity: String,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub file: Option<String>,
    #[serde(default)]
    pub line_start: Option<u32>,
    #[serde(default)]
    pub line_end: Option<u32>,
    #[serde(default)]
    pub side: Option<String>,
    #[serde(default)]
    pub explanation: String,
    #[serde(default)]
    pub proposed_comment: String,
    #[serde(default)]
    pub suggestion: Option<String>,
}

pub fn review_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["verdict", "summary", "findings", "critical_files"],
        "properties": {
            "verdict": {
                "type": "string",
                "enum": ["approve", "request_changes", "comment"],
                "description": "approve: no critical/high/medium issues remain. request_changes: at least one critical or high issue. comment: only medium/low/nit feedback."
            },
            "summary": {
                "type": "string",
                "description": "Two to five sentences for the PR author, in the configured comment language. Posted as the review body."
            },
            "findings": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["severity", "category", "title", "file", "line_start", "side", "explanation", "proposed_comment"],
                    "properties": {
                        "severity": { "type": "string", "enum": ["critical", "high", "medium", "low", "nit"] },
                        "category": {
                            "type": "string",
                            "description": "One of: bug, security, performance, concurrency, error-handling, api-design, maintainability, testing, documentation, style, scope (only for requirements from a linked ticket)."
                        },
                        "title": { "type": "string", "description": "Short headline in English for the reviewer UI." },
                        "file": { "type": ["string", "null"], "description": "Repo-relative path, or null for PR-wide findings." },
                        "line_start": { "type": ["integer", "null"], "description": "Line in the new file (side right) or old file (side left)." },
                        "line_end": { "type": ["integer", "null"], "description": "Last line for multi-line findings, else null." },
                        "side": { "type": "string", "enum": ["right", "left"], "description": "left only for lines that the PR removes." },
                        "explanation": { "type": "string", "description": "English. Why this is a problem, with evidence, for the person running the review." },
                        "proposed_comment": { "type": "string", "description": "The PR comment, addressed to the author, in the configured comment language. Markdown allowed." },
                        "suggestion": { "type": ["string", "null"], "description": "Optional exact replacement code for line_start..line_end (no fences)." }
                    }
                }
            },
            "critical_files": {
                "type": "array",
                "description": "Zero to five files changed in this PR that the person running the review should read personally, most important first.",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["path", "reason", "focus_lines"],
                    "properties": {
                        "path": { "type": "string", "description": "Repo-relative path of a file changed in this PR (new path for renames)." },
                        "reason": { "type": "string", "description": "English, one to three sentences: why a human should read this file and what to look at." },
                        "focus_lines": {
                            "type": "array",
                            "description": "Line ranges in the new file that deserve the closest look. May be empty.",
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["start", "end"],
                                "properties": {
                                    "start": { "type": "integer" },
                                    "end": { "type": "integer" }
                                }
                            }
                        }
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_valid_json_and_matches_structs() {
        let schema = review_schema();
        assert_eq!(schema["properties"]["verdict"]["enum"][0], "approve");
        let sample = serde_json::json!({
            "verdict": "comment",
            "summary": "s",
            "findings": [{
                "severity": "low", "category": "style", "title": "t", "file": null,
                "line_start": null, "side": "right", "explanation": "e", "proposed_comment": "c"
            }]
        });
        let parsed: RawReview = serde_json::from_value(sample).unwrap();
        assert_eq!(parsed.findings.len(), 1);
        assert_eq!(parsed.findings[0].file, None);
        assert!(parsed.critical_files.is_empty());

        let with_files: RawReview = serde_json::from_value(serde_json::json!({
            "verdict": "comment", "summary": "s", "findings": [],
            "critical_files": [{"path": "a.rs", "reason": "r", "focus_lines": [{"start": 1, "end": 4}]}]
        }))
        .unwrap();
        assert_eq!(with_files.critical_files[0].path, "a.rs");
        assert_eq!(with_files.critical_files[0].focus_lines[0].end, 4);
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "critical_files"));
    }
}
