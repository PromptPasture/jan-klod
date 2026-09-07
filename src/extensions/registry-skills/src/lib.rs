//! `registry-skills` — discovers Markdown skill files from `.agents/skills/`,
//! exposes them as `skill-registry` tools, and renders them on `invoke`.
//!
//! Each skill file must have a YAML front matter block (`--- ... ---`) at the
//! top containing at least a `name:` field and optionally a `description:` field.
//! The rest of the file is the skill template; `invoke` returns it (with
//! the raw JSON arguments appended as context).

#[allow(
    unsafe_code,
    missing_docs,
    clippy::all,
    clippy::pedantic,
    clippy::nursery
)]
mod bindings {
    wit_bindgen::generate!({
        world: "skill-registry-world",
        path: "../../../wit",
    });
}

use std::cell::RefCell;

use bindings::exports::jan_klod::interfaces::extension_lifecycle::{
    ExtensionContext, Guest as Lifecycle, HealthStatus,
};
use bindings::exports::jan_klod::interfaces::skill_registry::{
    Guest as SkillRegistry, SkillError, SkillInfo,
};
use bindings::jan_klod::interfaces::host_fs;
use bindings::jan_klod::interfaces::host_log::{self, LogLevel};

/// Resolved skill metadata + template body.
#[derive(Clone)]
struct Skill {
    name: String,
    description: String,
    path: String,
    body: String,
}

thread_local! {
    static SKILLS: RefCell<Vec<Skill>> = const { RefCell::new(Vec::new()) };
}

fn log(level: LogLevel, message: &str) {
    host_log::log(level, "registry-skills", message, &[]);
}

/// Parse YAML front matter from `content`. Returns `(front_matter, body)`.
///
/// Front matter is the text between the opening `---` and closing `---` lines.
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let rest = content.strip_prefix("---")?;
    // Allow `---\n` or `---\r\n`
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))?;
    let end = rest.find("\n---")?;
    let front = &rest[..end];
    let after_close = &rest[end + 4..]; // skip "\n---"
    let body = after_close
        .strip_prefix('\n')
        .or_else(|| after_close.strip_prefix("\r\n"))
        .unwrap_or(after_close);
    Some((front, body))
}

/// Extract `key: value` from a minimal YAML block (single-level only).
fn yaml_str<'a>(front: &'a str, key: &str) -> Option<&'a str> {
    for line in front.lines() {
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(val) = rest.strip_prefix(':') {
                return Some(val.trim().trim_matches('"').trim_matches('\''));
            }
        }
    }
    None
}

/// Scan `.agents/skills/` and load all Markdown files with a `name:` front matter.
fn load_skills() -> Vec<Skill> {
    let Ok(entries) = host_fs::list_dir(".agents/skills") else {
        return Vec::new();
    };
    let mut skills = Vec::new();
    for entry in entries {
        if entry.is_dir {
            continue;
        }
        let ext = entry.name.rsplit('.').next().unwrap_or("");
        if !ext.eq_ignore_ascii_case("md") {
            continue;
        }
        let path = format!(".agents/skills/{}", entry.name);
        let Ok(content) = host_fs::read(&path) else {
            continue;
        };
        let Some((front, body)) = split_frontmatter(&content) else {
            continue;
        };
        let name = match yaml_str(front, "name") {
            Some(n) if !n.is_empty() => n.to_owned(),
            _ => continue,
        };
        let description = yaml_str(front, "description").unwrap_or("").to_owned();
        skills.push(Skill {
            name,
            description,
            path: path.clone(),
            body: body.to_owned(),
        });
        log(LogLevel::Debug, &format!("loaded skill from {path}"));
    }
    skills
}

struct Component;

impl Lifecycle for Component {
    fn init(ctx: ExtensionContext) -> Result<(), String> {
        let skills = load_skills();
        let count = skills.len();
        SKILLS.with(|s| *s.borrow_mut() = skills);
        log(
            LogLevel::Info,
            &format!("init id={} skills={count}", ctx.id),
        );
        Ok(())
    }

    fn start() -> Result<(), String> {
        log(LogLevel::Info, "started");
        Ok(())
    }

    fn stop() {
        SKILLS.with(|s| s.borrow_mut().clear());
    }

    fn health() -> HealthStatus {
        HealthStatus::Up
    }
}

impl SkillRegistry for Component {
    fn list_skills() -> Result<Vec<SkillInfo>, SkillError> {
        Ok(SKILLS.with(|s| {
            s.borrow()
                .iter()
                .map(|sk| SkillInfo {
                    name: sk.name.clone(),
                    description: sk.description.clone(),
                    path: sk.path.clone(),
                    arguments_schema: "{}".to_owned(),
                })
                .collect()
        }))
    }

    fn get_skill(name: String) -> Result<SkillInfo, SkillError> {
        SKILLS.with(|s| {
            s.borrow()
                .iter()
                .find(|sk| sk.name == name)
                .map(|sk| SkillInfo {
                    name: sk.name.clone(),
                    description: sk.description.clone(),
                    path: sk.path.clone(),
                    arguments_schema: "{}".to_owned(),
                })
                .ok_or(SkillError::NotFound)
        })
    }

    fn invoke(name: String, arguments: String) -> Result<String, SkillError> {
        SKILLS
            .with(|s| {
                s.borrow()
                    .iter()
                    .find(|sk| sk.name == name)
                    .map(|sk| sk.body.clone())
            })
            .map_or(Err(SkillError::NotFound), |b| {
                let result = if arguments.is_empty() || arguments == "{}" {
                    b
                } else {
                    format!("{b}\n\n<!-- arguments: {arguments} -->")
                };
                Ok(result)
            })
    }

    fn reload() -> Result<(), SkillError> {
        let skills = load_skills();
        SKILLS.with(|s| *s.borrow_mut() = skills);
        Ok(())
    }
}

#[allow(
    unsafe_code,
    missing_docs,
    clippy::all,
    clippy::pedantic,
    clippy::nursery
)]
mod glue {
    use crate::{bindings, Component};
    bindings::export!(Component with_types_in bindings);
}
