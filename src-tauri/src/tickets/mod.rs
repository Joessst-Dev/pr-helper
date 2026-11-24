//! Issue tracker tickets (Jira) as review context.
//!
//! The app fetches the tickets itself, before the review, and hands them to the reviewer as a
//! file in the context dir. The reviewer never gets network or MCP access for this.
//!
//! Ticket keys are taken from author-controlled text (branch, title, description), so only
//! keys from explicitly configured projects are fetched, and every key is validated against
//! [`KEY_RE`] before it goes into a URL or prompt.

pub mod jira;
pub mod mcp;
pub mod render;

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// At most this many tickets are loaded for one PR.
pub const MAX_TICKETS: usize = 3;
/// The last N comments of a ticket are included.
pub const MAX_COMMENTS: usize = 10;
/// Ticket text is cut to these many characters, whichever fetcher loaded it.
pub const MAX_DESCRIPTION: usize = 8_000;
pub const MAX_FIELD: usize = 4_000;
pub const MAX_COMMENT: usize = 1_500;

static KEY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Z][A-Z0-9_]{0,49}-[1-9][0-9]{0,9}$").expect("hard-coded regex is valid")
});
/// Finds candidate keys in free text. Case-insensitive; the result is uppercased and then
/// validated with [`KEY_RE`].
static FIND_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:^|[^A-Za-z0-9_])([A-Za-z][A-Za-z0-9_]{0,49}-[1-9][0-9]{0,9})(?:$|[^A-Za-z0-9_])",
    )
    .expect("hard-coded regex is valid")
});

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TicketComment {
    pub author: String,
    pub created: String,
    pub body: String,
}

/// How the free text of a ticket (description, fields, comments) is written.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum TicketTextFormat {
    /// Jira wiki markup, as the REST v2 API returns it.
    JiraWiki,
    /// Markdown or plain text, e.g. from an MCP server.
    #[default]
    Markdown,
}

/// An image attached to a ticket. The bytes are fetched on demand through the app.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct TicketAttachment {
    pub id: String,
    pub filename: String,
    pub mime_type: String,
    #[ts(type = "number")]
    pub size: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Ticket {
    pub key: String,
    pub url: Option<String>,
    pub summary: String,
    pub issue_type: String,
    pub status: String,
    pub description: String,
    /// `KEY: summary` of the parent issue or epic.
    pub parent: Option<String>,
    /// Configured extra fields as (display name, text), e.g. acceptance criteria.
    pub fields: Vec<(String, String)>,
    /// The most recent comments, oldest first.
    pub comments: Vec<TicketComment>,
    /// Total number of comments on the ticket.
    #[ts(type = "number")]
    pub comment_total: usize,
    pub text_format: TicketTextFormat,
    /// Image attachments, which the text can embed by file name.
    pub attachments: Vec<TicketAttachment>,
}

pub fn is_valid_key(key: &str) -> bool {
    KEY_RE.is_match(key)
}

/// `"abc, PAY  ops"` → `["ABC", "PAY", "OPS"]`.
pub fn parse_project_keys(raw: &str) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for k in raw.split([',', ' ', ';', '\n', '\t']) {
        let k = k.trim().to_ascii_uppercase();
        if !k.is_empty() && !keys.contains(&k) {
            keys.push(k);
        }
    }
    keys
}

/// Ticket keys in the branch name, then the title, then the description, limited to the
/// allowed projects, deduplicated, at most [`MAX_TICKETS`].
pub fn extract_keys(
    head_ref: &str,
    title: &str,
    description: &str,
    allowed_projects: &[String],
) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for text in [head_ref, title, description] {
        let mut rest = text;
        // FIND_RE consumes one delimiter on each side, so step past each match manually to
        // also catch adjacent keys like "ABC-1,ABC-2".
        while let Some(m) = FIND_RE.captures(rest).and_then(|caps| caps.get(1)) {
            let key = m.as_str().to_ascii_uppercase();
            let project = key.rsplit_once('-').map_or("", |(p, _)| p);
            if is_valid_key(&key)
                && allowed_projects.iter().any(|p| p == project)
                && !keys.contains(&key)
            {
                keys.push(key);
                if keys.len() == MAX_TICKETS {
                    return keys;
                }
            }
            rest = &rest[m.end()..];
        }
    }
    keys
}

/// Keys requested from outside (the UI): uppercased, validated with [`KEY_RE`], limited to
/// the allowed projects, deduplicated, at most [`MAX_TICKETS`].
pub fn filter_keys(requested: &[String], allowed_projects: &[String]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for key in requested {
        let key = key.trim().to_ascii_uppercase();
        let project = key.rsplit_once('-').map_or("", |(p, _)| p);
        if is_valid_key(&key)
            && allowed_projects.iter().any(|p| p == project)
            && !keys.contains(&key)
        {
            keys.push(key);
            if keys.len() == MAX_TICKETS {
                break;
            }
        }
    }
    keys
}

/// Image types the app displays from ticket attachments. No SVG.
pub const IMAGE_MIME_TYPES: &[&str] = &[
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/webp",
    "image/bmp",
];

pub fn is_displayable_image(mime_type: &str) -> bool {
    IMAGE_MIME_TYPES.contains(&mime_type.trim().to_ascii_lowercase().as_str())
}

/// `{base}/browse/{key}`, if the base URL is known.
pub fn browse_url(base_url: &str, key: &str) -> Option<String> {
    let base = base_url.trim().trim_end_matches('/');
    (!base.is_empty()).then(|| format!("{base}/browse/{key}"))
}

/// Cut `text` to at most `max` chars, marking the cut.
pub fn truncate(text: &str, max: usize) -> String {
    let text = text.trim();
    match text.char_indices().nth(max) {
        Some((i, _)) => format!("{}\n[truncated]", text[..i].trim_end()),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allow(keys: &[&str]) -> Vec<String> {
        keys.iter().map(ToString::to_string).collect()
    }

    #[test]
    fn keys_come_from_branch_title_and_description_in_order() {
        let keys = extract_keys(
            "feature/abc-123-login",
            "PAY-7: fix rounding",
            "Relates to ABC-9 and ABC-123. See also OTHER-1.",
            &allow(&["ABC", "PAY"]),
        );
        assert_eq!(keys, vec!["ABC-123", "PAY-7", "ABC-9"]);
    }

    #[test]
    fn only_allowed_projects_and_at_most_three() {
        assert!(extract_keys("feature/SECRET-1", "", "", &allow(&["ABC"])).is_empty());
        assert!(extract_keys("feature/ABC-1", "", "", &[]).is_empty());
        let keys = extract_keys("", "ABC-1,ABC-2 ABC-3 ABC-4", "", &allow(&["ABC"]));
        assert_eq!(keys, vec!["ABC-1", "ABC-2", "ABC-3"]);
    }

    #[test]
    fn ignores_lookalikes() {
        let allowed = allow(&["ABC", "UTF"]);
        assert!(extract_keys("", "XABC-1 ABC-0 ABC-12x", "", &allowed).is_empty());
        assert_eq!(
            extract_keys("", "uses utf-8 now", "", &allowed),
            vec!["UTF-8"]
        );
    }

    #[test]
    fn requested_keys_are_validated_and_allow_listed() {
        let requested: Vec<String> = [
            "abc-1",
            "SECRET-2",
            "ABC-1",
            "ABC-0",
            "ABC-1/../x",
            "ABC-2",
            "ABC-3",
            "ABC-4",
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(
            filter_keys(&requested, &allow(&["ABC"])),
            vec!["ABC-1", "ABC-2", "ABC-3"]
        );
        assert!(filter_keys(&requested, &[]).is_empty());
    }

    #[test]
    fn key_validation() {
        assert!(is_valid_key("ABC-1"));
        assert!(is_valid_key("A2_B-99"));
        for bad in [
            "abc-1",
            "ABC-0",
            "ABC-1/../x",
            "ABC",
            "-1",
            "ABC-1 ",
            "1A-2",
        ] {
            assert!(!is_valid_key(bad), "{bad}");
        }
    }

    #[test]
    fn project_keys_parse_loosely() {
        assert_eq!(
            parse_project_keys(" abc, PAY;ops\nabc "),
            vec!["ABC", "PAY", "OPS"]
        );
        assert!(parse_project_keys("  ").is_empty());
    }

    #[test]
    fn truncation_is_char_safe() {
        assert_eq!(truncate("äöü", 5), "äöü");
        assert_eq!(truncate("äöüß", 2), "äö\n[truncated]");
        assert_eq!(
            browse_url("https://j/", "A-1").as_deref(),
            Some("https://j/browse/A-1")
        );
        assert_eq!(browse_url(" ", "A-1"), None);
    }
}
