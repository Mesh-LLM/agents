//! Skill discovery and installation planning for mesh-llm agents.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const SKILL_DIR_NAME: &str = "skill";
pub const SKILL_FILE_NAME: &str = "SKILL.md";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum SkillTarget {
    Codex,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum InstallMode {
    Copy,
    Link,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillDescriptor {
    pub agent_id: String,
    pub source_dir: PathBuf,
    pub skill_file: PathBuf,
}

impl SkillDescriptor {
    pub fn from_agent_dir(
        agent_id: impl Into<String>,
        agent_dir: impl AsRef<Path>,
    ) -> Result<Self> {
        let agent_id = agent_id.into();
        let source_dir = agent_dir.as_ref().join(SKILL_DIR_NAME);
        let skill_file = source_dir.join(SKILL_FILE_NAME);
        if !skill_file.is_file() {
            bail!("agent {agent_id} does not provide {SKILL_DIR_NAME}/{SKILL_FILE_NAME}");
        }
        Ok(Self {
            agent_id,
            source_dir,
            skill_file,
        })
    }

    #[must_use]
    pub fn default_codex_install_name(&self) -> String {
        format!("mesh-llm-{}", self.agent_id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillInstallPlan {
    pub descriptor: SkillDescriptor,
    pub target: SkillTarget,
    pub mode: InstallMode,
    pub destination_dir: PathBuf,
}

impl SkillInstallPlan {
    pub fn codex(
        descriptor: SkillDescriptor,
        codex_skills_dir: impl AsRef<Path>,
        mode: InstallMode,
    ) -> Self {
        let destination_dir = codex_skills_dir
            .as_ref()
            .join(descriptor.default_codex_install_name());
        Self {
            descriptor,
            target: SkillTarget::Codex,
            mode,
            destination_dir,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillInstallResult {
    pub destination_dir: PathBuf,
    pub replaced_existing: bool,
}

pub fn install_skill(plan: &SkillInstallPlan, force: bool) -> Result<SkillInstallResult> {
    let replaced_existing = plan.destination_dir.exists();
    if replaced_existing {
        if !force {
            bail!(
                "skill destination {} already exists; pass --force to replace it",
                plan.destination_dir.display()
            );
        }
        fs::remove_dir_all(&plan.destination_dir).with_context(|| {
            format!(
                "failed to remove existing skill {}",
                plan.destination_dir.display()
            )
        })?;
    }

    if let Some(parent) = plan.destination_dir.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    match plan.mode {
        InstallMode::Copy => copy_dir_all(&plan.descriptor.source_dir, &plan.destination_dir)?,
        InstallMode::Link => symlink_dir(&plan.descriptor.source_dir, &plan.destination_dir)?,
    }

    Ok(SkillInstallResult {
        destination_dir: plan.destination_dir.clone(),
        replaced_existing,
    })
}

fn copy_dir_all(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)
        .with_context(|| format!("failed to create {}", destination.display()))?;
    for entry in
        fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
    {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_all(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path).with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    source_path.display(),
                    destination_path.display()
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn symlink_dir(source: &Path, destination: &Path) -> Result<()> {
    std::os::unix::fs::symlink(source, destination).with_context(|| {
        format!(
            "failed to symlink {} to {}",
            source.display(),
            destination.display()
        )
    })
}

#[cfg(windows)]
fn symlink_dir(source: &Path, destination: &Path) -> Result<()> {
    std::os::windows::fs::symlink_dir(source, destination).with_context(|| {
        format!(
            "failed to symlink {} to {}",
            source.display(),
            destination.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn descriptor_requires_skill_file() {
        let root =
            std::env::temp_dir().join(format!("mesh-agents-skills-missing-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create temp skill directory");

        let error = SkillDescriptor::from_agent_dir("agent", &root)
            .expect_err("missing skill should be rejected")
            .to_string();

        assert!(error.contains("does not provide"));
    }

    #[test]
    fn plans_codex_destination() {
        let root =
            std::env::temp_dir().join(format!("mesh-agents-skills-present-{}", std::process::id()));
        let skill_dir = root.join(SKILL_DIR_NAME);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&skill_dir).expect("create source skill directory");
        fs::write(skill_dir.join(SKILL_FILE_NAME), "---\nname: test\n---\n")
            .expect("write source skill");

        let descriptor =
            SkillDescriptor::from_agent_dir("pr-review", &root).expect("load skill descriptor");
        let plan = SkillInstallPlan::codex(descriptor, "/tmp/codex-skills", InstallMode::Copy);

        assert_eq!(
            plan.destination_dir,
            PathBuf::from("/tmp/codex-skills/mesh-llm-pr-review")
        );
    }

    #[test]
    fn installs_skill_by_copy() {
        let root =
            std::env::temp_dir().join(format!("mesh-agents-skills-install-{}", std::process::id()));
        let agent_dir = root.join("agent");
        let source = agent_dir.join(SKILL_DIR_NAME);
        let target = root.join("target");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&source).expect("create source skill directory");
        fs::write(source.join(SKILL_FILE_NAME), "---\nname: test\n---\n")
            .expect("write source skill");

        let descriptor = SkillDescriptor::from_agent_dir("pr-review", &agent_dir)
            .expect("load skill descriptor");
        let plan = SkillInstallPlan::codex(descriptor, &target, InstallMode::Copy);
        let result = install_skill(&plan, false).expect("install skill by copy");

        assert!(!result.replaced_existing);
        assert!(result.destination_dir.join(SKILL_FILE_NAME).is_file());
    }
}
