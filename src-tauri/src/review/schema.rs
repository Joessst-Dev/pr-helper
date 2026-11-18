//! The JSON shape the reviewer must return (passed to `claude --json-schema`).

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct RawReview {
    pub verdict: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub findings: Vec<RawFinding>,
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
        "required": ["verdict", "summary", "findings"],
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
                            "description": "One of: bug, security, performance, concurrency, error-handling, api-design, maintainability, testing, documentation, style."
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
    }
}
