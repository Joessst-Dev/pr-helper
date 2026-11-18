//! Review behaviour settings, resolved defaults -> global -> server -> repo.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::db::models::Severity;

/// Every field is optional so each level only stores what it overrides.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct SettingsOverrides {
    pub comment_language: Option<String>,
    pub forbid_ai_attribution: Option<bool>,
    /// Footer appended to posted reviews when attribution is allowed. Empty = no footer.
    pub attribution_footer: Option<String>,
    pub skip_nits: Option<bool>,
    pub auto_review_on_new_commits: Option<bool>,
    pub auto_approve_on_approve_verdict: Option<bool>,
    pub poll_interval_secs: Option<u32>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub max_budget_usd: Option<f64>,
    pub default_selected_min_severity: Option<Severity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export)]
pub enum SettingSource {
    Default,
    Global,
    Server,
    Repo,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct Sourced<T> {
    pub value: T,
    pub source: SettingSource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export)]
pub struct EffectiveSettings {
    pub comment_language: Sourced<String>,
    pub forbid_ai_attribution: Sourced<bool>,
    pub attribution_footer: Sourced<String>,
    pub skip_nits: Sourced<bool>,
    pub auto_review_on_new_commits: Sourced<bool>,
    pub auto_approve_on_approve_verdict: Sourced<bool>,
    pub poll_interval_secs: Sourced<u32>,
    pub model: Sourced<Option<String>>,
    pub effort: Sourced<Option<String>>,
    pub max_budget_usd: Sourced<Option<f64>>,
    pub default_selected_min_severity: Sourced<Severity>,
}

pub const MIN_POLL_INTERVAL_SECS: u32 = 60;

fn pick<T: Clone>(default: T, levels: [(SettingSource, Option<&T>); 3]) -> Sourced<T> {
    let mut out = Sourced {
        value: default,
        source: SettingSource::Default,
    };
    for (source, value) in levels {
        if let Some(v) = value {
            out = Sourced {
                value: v.clone(),
                source,
            };
        }
    }
    out
}

/// Like [`pick`], for settings whose default is "unset" (e.g. use the CLI's default model).
fn pick_opt<T: Clone>(levels: [(SettingSource, Option<&T>); 3]) -> Sourced<Option<T>> {
    let mut out = Sourced {
        value: None,
        source: SettingSource::Default,
    };
    for (source, value) in levels {
        if let Some(v) = value {
            out = Sourced {
                value: Some(v.clone()),
                source,
            };
        }
    }
    out
}

pub fn resolve(
    global: &SettingsOverrides,
    server: &SettingsOverrides,
    repo: Option<&SettingsOverrides>,
) -> EffectiveSettings {
    let empty = SettingsOverrides::default();
    let repo = repo.unwrap_or(&empty);
    macro_rules! levels {
        ($field:ident) => {
            [
                (SettingSource::Global, global.$field.as_ref()),
                (SettingSource::Server, server.$field.as_ref()),
                (SettingSource::Repo, repo.$field.as_ref()),
            ]
        };
    }

    let mut poll = pick(300, levels!(poll_interval_secs));
    poll.value = poll.value.max(MIN_POLL_INTERVAL_SECS);

    EffectiveSettings {
        comment_language: pick("English".to_string(), levels!(comment_language)),
        forbid_ai_attribution: pick(true, levels!(forbid_ai_attribution)),
        attribution_footer: pick(String::new(), levels!(attribution_footer)),
        skip_nits: pick(false, levels!(skip_nits)),
        auto_review_on_new_commits: pick(false, levels!(auto_review_on_new_commits)),
        auto_approve_on_approve_verdict: pick(false, levels!(auto_approve_on_approve_verdict)),
        poll_interval_secs: poll,
        model: pick_opt(levels!(model)),
        effort: pick_opt(levels!(effort)),
        max_budget_usd: pick_opt(levels!(max_budget_usd)),
        default_selected_min_severity: pick(
            Severity::Medium,
            levels!(default_selected_min_severity),
        ),
    }
}

/// App-wide configuration that is not part of the per-server hierarchy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export)]
pub struct AppConfig {
    /// Explicit path to the `claude` binary. `None` = discover via login shell PATH.
    pub claude_path: Option<String>,
    pub git_path: Option<String>,
    pub max_concurrent_reviews: u32,
    /// Hard stop for a single review run.
    pub review_timeout_mins: u32,
    pub launch_at_login: bool,
    /// Keep the worktree after a review so the session can be resumed in a terminal.
    pub keep_worktrees: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            claude_path: None,
            git_path: None,
            max_concurrent_reviews: 2,
            review_timeout_mins: 30,
            launch_at_login: false,
            keep_worktrees: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_apply_when_nothing_is_set() {
        let empty = SettingsOverrides::default();
        let eff = resolve(&empty, &empty, None);
        assert_eq!(eff.comment_language.value, "English");
        assert_eq!(eff.comment_language.source, SettingSource::Default);
        assert!(eff.forbid_ai_attribution.value);
        assert!(!eff.skip_nits.value);
        assert_eq!(eff.poll_interval_secs.value, 300);
        assert_eq!(eff.model.value, None);
        assert_eq!(eff.default_selected_min_severity.value, Severity::Medium);
    }

    #[test]
    fn most_specific_level_wins_and_reports_its_source() {
        let global = SettingsOverrides {
            comment_language: Some("German".into()),
            skip_nits: Some(true),
            model: Some("opus".into()),
            ..Default::default()
        };
        let server = SettingsOverrides {
            comment_language: Some("French".into()),
            ..Default::default()
        };
        let repo = SettingsOverrides {
            skip_nits: Some(false),
            ..Default::default()
        };
        let eff = resolve(&global, &server, Some(&repo));
        assert_eq!(eff.comment_language.value, "French");
        assert_eq!(eff.comment_language.source, SettingSource::Server);
        assert!(!eff.skip_nits.value);
        assert_eq!(eff.skip_nits.source, SettingSource::Repo);
        assert_eq!(eff.model.value.as_deref(), Some("opus"));
        assert_eq!(eff.model.source, SettingSource::Global);
    }

    #[test]
    fn poll_interval_is_clamped() {
        let server = SettingsOverrides {
            poll_interval_secs: Some(5),
            ..Default::default()
        };
        let eff = resolve(&SettingsOverrides::default(), &server, None);
        assert_eq!(eff.poll_interval_secs.value, MIN_POLL_INTERVAL_SECS);
    }

    #[test]
    fn overrides_deserialize_from_partial_json() {
        let o: SettingsOverrides = serde_json::from_str(r#"{"skipNits":true}"#).unwrap();
        assert_eq!(o.skip_nits, Some(true));
        assert_eq!(o.comment_language, None);
    }
}
