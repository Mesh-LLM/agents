use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::{CodexCommand, SkillsCommand};

const SKILL_NAME: &str = "mesh-agents";
const MCP_SERVER_NAME: &str = "mesh_a2a";
const CONFIG_BEGIN: &str = "# BEGIN mesh-agents";
const CONFIG_END: &str = "# END mesh-agents";

pub(crate) fn dispatch_codex_command(command: &CodexCommand) -> Result<()> {
    match command {
        CodexCommand::Setup {
            config,
            skills_dir,
            mcp_command,
            dry_run,
        } => setup_codex(
            config.as_deref(),
            skills_dir.as_deref(),
            mcp_command,
            *dry_run,
        ),
    }
}

pub(crate) fn dispatch_skills_command(command: &SkillsCommand) -> Result<()> {
    match command {
        SkillsCommand::List => {
            println!("{SKILL_NAME}");
            Ok(())
        }
        SkillsCommand::Install {
            skill,
            codex_dir,
            force,
        } => {
            require_mesh_agents_skill(skill)?;
            let destination = install_mesh_agents_skill(codex_dir.as_deref(), *force)?;
            println!("installed {SKILL_NAME} skill at {}", destination.display());
            Ok(())
        }
        SkillsCommand::Uninstall { skill, codex_dir } => {
            require_mesh_agents_skill(skill)?;
            let destination = codex_skills_dir(codex_dir.as_deref())?.join(SKILL_NAME);
            if destination.exists() {
                fs::remove_dir_all(&destination)
                    .with_context(|| format!("failed to remove {}", destination.display()))?;
            }
            println!("removed {SKILL_NAME} skill from {}", destination.display());
            Ok(())
        }
        SkillsCommand::Status { codex_dir } => {
            let destination = codex_skills_dir(codex_dir.as_deref())?.join(SKILL_NAME);
            println!("skill\tinstalled\tdestination");
            println!(
                "{SKILL_NAME}\t{}\t{}",
                destination.join("SKILL.md").is_file(),
                destination.display()
            );
            Ok(())
        }
    }
}

fn setup_codex(
    config: Option<&Path>,
    skills_dir: Option<&Path>,
    mcp_command: &str,
    dry_run: bool,
) -> Result<()> {
    let config_path = codex_config_path(config)?;
    let skill_destination = codex_skills_dir(skills_dir)?.join(SKILL_NAME);
    let config_body = managed_mcp_config(mcp_command);

    if dry_run {
        println!("Would write skill: {}", skill_destination.display());
        println!("Would update config: {}", config_path.display());
        println!("{config_body}");
        return Ok(());
    }

    install_mesh_agents_skill(skills_dir, true)?;
    upsert_managed_config_block(&config_path, &config_body)?;
    println!("configured Codex MCP at {}", config_path.display());
    println!(
        "installed {SKILL_NAME} skill at {}",
        skill_destination.display()
    );
    Ok(())
}

fn require_mesh_agents_skill(skill: &str) -> Result<()> {
    if skill != SKILL_NAME {
        bail!("unknown skill `{skill}`; expected `{SKILL_NAME}`");
    }
    Ok(())
}

fn install_mesh_agents_skill(codex_dir: Option<&Path>, force: bool) -> Result<PathBuf> {
    let destination = codex_skills_dir(codex_dir)?.join(SKILL_NAME);
    if destination.exists() {
        if !force {
            bail!(
                "skill destination {} already exists; pass --force to replace it",
                destination.display()
            );
        }
        fs::remove_dir_all(&destination)
            .with_context(|| format!("failed to remove {}", destination.display()))?;
    }
    fs::create_dir_all(&destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    fs::write(destination.join("SKILL.md"), mesh_agents_skill())
        .with_context(|| format!("failed to write {}", destination.join("SKILL.md").display()))?;
    Ok(destination)
}

fn upsert_managed_config_block(config_path: &Path, block: &str) -> Result<()> {
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let existing = fs::read_to_string(config_path).unwrap_or_default();
    let next = replace_managed_block(&existing, block);
    fs::write(config_path, next)
        .with_context(|| format!("failed to write {}", config_path.display()))
}

fn replace_managed_block(existing: &str, block: &str) -> String {
    let managed = format!("{CONFIG_BEGIN}\n{block}\n{CONFIG_END}\n");
    let Some(begin) = existing.find(CONFIG_BEGIN) else {
        let separator = if existing.trim().is_empty() || existing.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        return format!("{existing}{separator}{managed}");
    };
    let Some(end_relative) = existing[begin..].find(CONFIG_END) else {
        return format!("{}\n{managed}", existing.trim_end());
    };
    let end = begin + end_relative + CONFIG_END.len();
    let mut next = String::new();
    next.push_str(&existing[..begin]);
    next.push_str(&managed);
    next.push_str(existing[end..].trim_start_matches('\n'));
    next
}

fn managed_mcp_config(mcp_command: &str) -> String {
    format!(
        r#"[mcp_servers.{MCP_SERVER_NAME}]
command = "{mcp_command}"
args = ["a2a", "mcp"]
enabled_tools = [
  "get_agents",
  "get_agent",
  "send_message",
  "get_task",
  "view_text_artifact",
  "view_data_artifact",
]
default_tools_approval_mode = "prompt""#
    )
}

fn mesh_agents_skill() -> &'static str {
    r#"---
name: mesh-agents
description: Discover and delegate work to mesh A2A agents through the mesh-agents MCP tools.
---

Use this skill when the user asks to find, delegate to, or inspect work from mesh agents.

Workflow:

- Call `get_agents` to discover available agents.
- Call `get_agent` before delegating if the task requires a specific capability.
- Call `send_message` with a clear task brief and relevant URLs or paths.
- Use `get_task` to inspect task state and final status.
- Use `view_text_artifact` or `view_data_artifact` when a task returns artifacts.

Agents are discovered dynamically through MCP. Do not assume a specific agent is installed locally.
"#
}

fn codex_config_path(path: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = path {
        return Ok(path.to_path_buf());
    }
    Ok(home_dir()?.join(".codex").join("config.toml"))
}

fn codex_skills_dir(path: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = path {
        return Ok(path.to_path_buf());
    }
    Ok(home_dir()?.join(".codex").join("skills"))
}

fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().context("could not determine home directory")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_existing_managed_block() {
        let existing = "alpha\n# BEGIN mesh-agents\nold\n# END mesh-agents\nomega\n";
        let next = replace_managed_block(existing, "new");

        assert!(next.contains("alpha\n# BEGIN mesh-agents\nnew\n# END mesh-agents\nomega"));
        assert!(!next.contains("old"));
    }
}
