//! Packages the configured skills into a throwaway Claude Code plugin (`--plugin-dir`).

use std::path::{Path, PathBuf};

use crate::db::models::Skill;
use crate::error::{AppError, AppResult};
use crate::review::prompt::SkillInfo;

pub const PLUGIN_NAME: &str = "pr-helper-review-skills";

/// Skill directory names must be simple slugs.
pub fn slug(name: &str) -> String {
    let s: String = name
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if s.is_empty() { "skill".into() } else { s }
}

/// Inline skills without frontmatter get one generated from the stored name and description.
/// `name` is the skill's directory in the plugin, which is the name the reviewer invokes.
fn with_frontmatter(skill: &Skill, body: &str, name: &str) -> String {
    if body.trim_start().starts_with("---") {
        return set_frontmatter_name(body, name);
    }
    let description = if skill.description.trim().is_empty() {
        format!("Review guidance: {}", skill.name)
    } else {
        skill.description.trim().replace('\n', " ")
    };
    format!(
        "---\nname: {}\ndescription: {}\n---\n\n{}",
        name,
        serde_json::to_string(&description).unwrap_or_default(),
        body
    )
}

/// Makes the frontmatter `name` match the plugin directory, so the name the prompt advertises
/// (`pr-helper-review-skills:<dir>`) is the one Claude Code registers, even after renaming a
/// duplicate or for a folder skill whose own name differs.
fn set_frontmatter_name(text: &str, name: &str) -> String {
    let body = text.trim_start();
    let Some(rest) = body.strip_prefix("---") else {
        return text.to_string();
    };
    let Some(end) = rest.find("\n---") else {
        return text.to_string();
    };
    let (front, after) = rest.split_at(end);
    let mut lines: Vec<String> = front
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect();
    match lines
        .iter_mut()
        .find(|l| l.trim_start().starts_with("name:"))
    {
        Some(line) => *line = format!("name: {name}"),
        None => lines.insert(0, format!("name: {name}")),
    }
    format!("---\n{}{after}", lines.join("\n"))
}

async fn copy_dir(src: &Path, dst: &Path) -> AppResult<()> {
    tokio::fs::create_dir_all(dst).await?;
    let mut stack = vec![(src.to_path_buf(), dst.to_path_buf())];
    while let Some((from, to)) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&from).await?;
        while let Some(entry) = entries.next_entry().await? {
            let ty = entry.file_type().await?;
            let target = to.join(entry.file_name());
            if ty.is_dir() {
                tokio::fs::create_dir_all(&target).await?;
                stack.push((entry.path(), target));
            } else if ty.is_file() {
                tokio::fs::copy(entry.path(), &target).await?;
            }
            // Symlinks are skipped so a skill folder can't pull in arbitrary files.
        }
    }
    Ok(())
}

/// Validate a skill folder chosen by the user.
pub fn validate_skill_dir(path: &Path) -> AppResult<()> {
    if path.join("SKILL.md").is_file() {
        Ok(())
    } else {
        Err(AppError::invalid(format!(
            "{} does not contain a SKILL.md",
            path.display()
        )))
    }
}

/// Writes the plugin and returns its directory plus what to tell the reviewer about each skill.
pub async fn build_plugin(
    skills: &[Skill],
    dest: &Path,
) -> AppResult<Option<(PathBuf, Vec<SkillInfo>)>> {
    if skills.is_empty() {
        return Ok(None);
    }
    let skills_dir = dest.join("skills");
    tokio::fs::create_dir_all(dest.join(".claude-plugin")).await?;
    tokio::fs::create_dir_all(&skills_dir).await?;
    tokio::fs::write(
        dest.join(".claude-plugin/plugin.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "name": PLUGIN_NAME,
            "version": "1.0.0",
            "description": "Review skills configured in PR Helper",
        }))?,
    )
    .await?;

    let mut infos = Vec::new();
    let mut used = std::collections::HashSet::new();
    for skill in skills {
        let mut name = slug(&skill.name);
        while !used.insert(name.clone()) {
            name.push_str("-x");
        }
        let target = skills_dir.join(&name);
        match (&skill.path, &skill.content) {
            (Some(path), _) => {
                let src = PathBuf::from(path);
                validate_skill_dir(&src)?;
                copy_dir(&src, &target).await?;
                let md = target.join("SKILL.md");
                let text = tokio::fs::read_to_string(&md).await?;
                tokio::fs::write(&md, set_frontmatter_name(&text, &name)).await?;
            }
            (None, Some(content)) => {
                tokio::fs::create_dir_all(&target).await?;
                tokio::fs::write(
                    target.join("SKILL.md"),
                    with_frontmatter(skill, content, &name),
                )
                .await?;
            }
            (None, None) => continue,
        }
        infos.push(SkillInfo {
            name: format!("{PLUGIN_NAME}:{name}"),
            description: if skill.description.trim().is_empty() {
                skill.name.clone()
            } else {
                skill.description.trim().to_string()
            },
        });
    }
    Ok(Some((dest.to_path_buf(), infos)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(name: &str, path: Option<String>, content: Option<&str>) -> Skill {
        Skill {
            id: "id".into(),
            server_id: "s".into(),
            repo: None,
            name: name.into(),
            description: "desc".into(),
            path,
            content: content.map(Into::into),
            enabled: true,
            created_at: String::new(),
        }
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Rust Safety!"), "rust-safety");
        assert_eq!(slug("  "), "skill");
    }

    #[tokio::test]
    async fn builds_plugin_from_inline_and_folder_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src-skill");
        std::fs::create_dir_all(src.join("refs")).unwrap();
        std::fs::write(
            src.join("SKILL.md"),
            "---\nname: x\ndescription: y\n---\nbody",
        )
        .unwrap();
        std::fs::write(src.join("refs/a.md"), "ref").unwrap();

        let dest = tmp.path().join("plugin");
        let skills = vec![
            skill("API Rules", None, Some("Check the API rules.")),
            skill("Folder Skill", Some(src.to_string_lossy().into()), None),
            skill("API Rules", None, Some("dupe name")),
        ];
        let (dir, infos) = build_plugin(&skills, &dest).await.unwrap().unwrap();
        assert!(dir.join(".claude-plugin/plugin.json").is_file());
        let inline = std::fs::read_to_string(dir.join("skills/api-rules/SKILL.md")).unwrap();
        assert!(inline.starts_with("---\nname: api-rules\ndescription: \"desc\""));
        assert!(dir.join("skills/folder-skill/refs/a.md").is_file());
        let folder = std::fs::read_to_string(dir.join("skills/folder-skill/SKILL.md")).unwrap();
        assert_eq!(folder, "---\nname: folder-skill\ndescription: y\n---\nbody");
        let dupe = std::fs::read_to_string(dir.join("skills/api-rules-x/SKILL.md")).unwrap();
        assert!(dupe.starts_with("---\nname: api-rules-x\n"));
        assert_eq!(infos[2].name, format!("{PLUGIN_NAME}:api-rules-x"));
        assert_eq!(infos[0].name, format!("{PLUGIN_NAME}:api-rules"));
        assert_eq!(infos.len(), 3);
    }

    #[tokio::test]
    async fn no_skills_means_no_plugin() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(build_plugin(&[], tmp.path()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rejects_folder_without_skill_md() {
        let tmp = tempfile::tempdir().unwrap();
        let skills = vec![skill(
            "bad",
            Some(tmp.path().to_string_lossy().into()),
            None,
        )];
        assert!(build_plugin(&skills, &tmp.path().join("p")).await.is_err());
    }
}
