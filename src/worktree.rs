use anyhow::{Context, Result};
use inquire::error::InquireError;
use inquire::Select;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{get_trees_dir, Config, LinkType};

pub fn get_current_branch(project_root: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(project_root)
        .output()
        .context("Failed to run git rev-parse")?;

    if !output.status.success() {
        anyhow::bail!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn branch_exists(project_root: &Path, branch_name: &str) -> Result<bool> {
    let output = Command::new("git")
        .args(["show-ref", "--verify", "--quiet", &format!("refs/heads/{}", branch_name)])
        .current_dir(project_root)
        .output()
        .context("Failed to run git show-ref")?;

    Ok(output.status.success())
}

pub fn is_path_tracked(project_root: &Path, path: &str) -> Result<bool> {
    let output = Command::new("git")
        .args(["ls-files", "--error-unmatch", "--", path])
        .current_dir(project_root)
        .output()
        .context("Failed to run git ls-files")?;

    Ok(output.status.success())
}

pub fn ensure_on_main_branch(project_root: &Path, main_branch: &str) -> Result<()> {
    let current = get_current_branch(project_root)?;
    if current != main_branch {
        anyhow::bail!(
            "Not on main branch. Current branch is '{}', expected '{}'. \
            Switch to the main branch before creating a worktree.",
            current,
            main_branch
        );
    }
    Ok(())
}

pub fn get_worktree_path(project_root: &Path, name: &str) -> Result<PathBuf> {
    let trees_dir = get_trees_dir(project_root);
    let worktree_path = trees_dir.join(name);

    if !worktree_path.exists() {
        anyhow::bail!(
            "Worktree '{}' does not exist.\n{}",
            name,
            format_worktree_list(project_root)?
        );
    }

    Ok(worktree_path)
}

/// Detect if the current directory is inside a worktree managed by epiphyte.
/// Returns the worktree name if found, None otherwise.
pub fn detect_current_worktree(project_root: &Path) -> Result<Option<String>> {
    let current_dir = std::env::current_dir().context("Failed to get current directory")?;
    let trees_dir = get_trees_dir(project_root);

    if !current_dir.starts_with(&trees_dir) {
        return Ok(None);
    }

    // Find the worktree root (direct child of trees_dir)
    let relative = current_dir
        .strip_prefix(&trees_dir)
        .context("Failed to get relative path")?;

    // Get the first component (worktree name)
    if let Some(first) = relative.components().next() {
        let name = first.as_os_str().to_string_lossy().to_string();
        // Verify it's actually a worktree
        if trees_dir.join(&name).exists() {
            return Ok(Some(name));
        }
    }

    Ok(None)
}

/// Get the worktree name, either from the provided argument or by detecting the current worktree.
pub fn resolve_worktree_name(project_root: &Path, name: Option<&str>) -> Result<String> {
    match name {
        Some(n) => Ok(n.to_string()),
        None => detect_current_worktree(project_root)?.ok_or_else(|| {
            anyhow::anyhow!(
                "Not inside a worktree. Please specify a worktree name.\n{}",
                format_worktree_list(project_root).unwrap_or_else(|err| {
                    format!("Failed to list worktrees: {}", err)
                })
            )
        }),
    }
}

pub fn enter_worktree(worktree_path: &Path) -> Result<()> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());

    let status = Command::new(&shell)
        .current_dir(worktree_path)
        .status()
        .with_context(|| format!("Failed to spawn shell: {}", shell))?;

    if !status.success() {
        if let Some(code) = status.code() {
            std::process::exit(code);
        }
    }

    Ok(())
}

#[derive(Clone)]
pub struct Worktree {
    pub name: String,
    pub path: PathBuf,
    pub branch: String,
}

impl std::fmt::Display for Worktree {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.branch.is_empty() {
            write!(f, "{}  {}", self.name, self.path.display())
        } else {
            write!(
                f,
                "{}  [{}]  {}",
                self.name,
                self.branch,
                self.path.display()
            )
        }
    }
}

pub fn list_worktrees(project_root: &Path) -> Result<Vec<Worktree>> {
    let trees_dir = get_trees_dir(project_root);
    let mut worktrees = Vec::new();

    if !trees_dir.exists() {
        return Ok(worktrees);
    }

    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(project_root)
        .output()
        .context("Failed to run git worktree list")?;

    if !output.status.success() {
        anyhow::bail!(
            "git worktree list failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut current_path: Option<PathBuf> = None;
    let mut current_branch: Option<String> = None;

    for line in stdout.lines() {
        if line.starts_with("worktree ") {
            if let Some(path) = current_path.take() {
                if path.starts_with(&trees_dir) {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    worktrees.push(Worktree {
                        name,
                        path: path.clone(),
                        branch: current_branch.take().unwrap_or_default(),
                    });
                }
            }
            current_path = Some(PathBuf::from(line.strip_prefix("worktree ").unwrap()));
            current_branch = None;
        } else if line.starts_with("branch ") {
            current_branch = Some(
                line.strip_prefix("branch refs/heads/")
                    .unwrap_or(line.strip_prefix("branch ").unwrap())
                    .to_string(),
            );
        }
    }

    // Handle the last entry
    if let Some(path) = current_path {
        if path.starts_with(&trees_dir) {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            worktrees.push(Worktree {
                name,
                path,
                branch: current_branch.unwrap_or_default(),
            });
        }
    }

    Ok(worktrees)
}

pub fn select_worktree_name(project_root: &Path) -> Result<Option<String>> {
    let worktrees = list_worktrees(project_root)?;
    if worktrees.is_empty() {
        anyhow::bail!("No worktrees found.");
    }
    if worktrees.len() == 1 {
        return Ok(Some(worktrees[0].name.clone()));
    }

    let selection = Select::new("Select worktree", worktrees).prompt();
    match selection {
        Ok(worktree) => Ok(Some(worktree.name)),
        Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => Ok(None),
        Err(err) => Err(err).context("Failed to prompt for worktree selection"),
    }
}

fn format_worktree_list(project_root: &Path) -> Result<String> {
    let worktrees = list_worktrees(project_root)?;
    if worktrees.is_empty() {
        return Ok("No worktrees found.".to_string());
    }

    let mut output = String::from("Current worktrees:\n");
    for wt in worktrees {
        output.push_str(&format!(
            "  {}\t{}\t{}\n",
            wt.name,
            wt.branch,
            wt.path.display()
        ));
    }
    Ok(output.trim_end().to_string())
}

pub fn add_worktree(
    project_root: &Path,
    name: &str,
    branch: Option<&str>,
    config: &Config,
) -> Result<PathBuf> {
    let trees_dir = get_trees_dir(project_root);
    fs::create_dir_all(&trees_dir)
        .with_context(|| format!("Failed to create trees dir: {}", trees_dir.display()))?;

    let worktree_path = trees_dir.join(name);
    if worktree_path.exists() {
        anyhow::bail!("Worktree '{}' already exists", name);
    }

    let worktree_path_str = worktree_path.to_string_lossy().to_string();

    // Determine the branch to use and whether to create a new one
    let (branch_name, create_new_branch) = if let Some(b) = branch {
        // Explicit branch specified - use it as-is (checkout existing)
        (b.to_string(), false)
    } else if branch_exists(project_root, name)? {
        // Branch with the same name as worktree already exists - checkout it
        (name.to_string(), false)
    } else {
        // No existing branch - create a new one
        (name.to_string(), true)
    };

    let args: Vec<&str> = if create_new_branch {
        vec!["worktree", "add", "-b", &branch_name, &worktree_path_str]
    } else {
        vec!["worktree", "add", &worktree_path_str, &branch_name]
    };

    let output = Command::new("git")
        .args(&args)
        .current_dir(project_root)
        .output()
        .context("Failed to run git worktree add")?;

    if !output.status.success() {
        anyhow::bail!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Link/copy configured files
    link_files(project_root, &worktree_path, config)?;

    Ok(worktree_path)
}

fn link_files(project_root: &Path, worktree_path: &Path, config: &Config) -> Result<()> {
    for entry in &config.files {
        let src = project_root.join(&entry.path);
        let dst = worktree_path.join(&entry.path);

        if !src.exists() {
            eprintln!("Warning: source file does not exist: {}", src.display());
            continue;
        }

        // Create parent directories for destination
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent dir: {}", parent.display()))?;
        }

        // Remove existing destination if it exists
        if dst.exists() || dst.symlink_metadata().is_ok() {
            if dst.is_dir() && !dst.symlink_metadata()?.file_type().is_symlink() {
                fs::remove_dir_all(&dst)?;
            } else {
                fs::remove_file(&dst)?;
            }
        }

        match entry.link_type {
            LinkType::Symlink => {
                #[cfg(unix)]
                {
                    std::os::unix::fs::symlink(&src, &dst).with_context(|| {
                        format!("Failed to symlink {} -> {}", src.display(), dst.display())
                    })?;
                }
                #[cfg(windows)]
                {
                    if src.is_dir() {
                        std::os::windows::fs::symlink_dir(&src, &dst)?;
                    } else {
                        std::os::windows::fs::symlink_file(&src, &dst)?;
                    }
                }
            }
            LinkType::Copy => {
                if src.is_dir() {
                    copy_dir_recursive(&src, &dst)?;
                } else {
                    fs::copy(&src, &dst).with_context(|| {
                        format!("Failed to copy {} -> {}", src.display(), dst.display())
                    })?;
                }
            }
        }
    }

    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

pub fn relink_worktree(project_root: &Path, name: &str, config: &Config) -> Result<()> {
    let trees_dir = get_trees_dir(project_root);
    let worktree_path = trees_dir.join(name);

    if !worktree_path.exists() {
        anyhow::bail!("Worktree '{}' does not exist", name);
    }

    link_files(project_root, &worktree_path, config)?;

    Ok(())
}
