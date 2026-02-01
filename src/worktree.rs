use anyhow::{Context, Result};
use inquire::error::InquireError;
use inquire::Select;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::{get_trees_dir, Config, FileEntry, LinkType};

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

pub fn list_ignored_files(project_root: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .args(["ls-files", "-i", "-o", "--exclude-standard"])
        .current_dir(project_root)
        .output()
        .context("Failed to run git ls-files")?;

    if !output.status.success() {
        anyhow::bail!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut files: Vec<String> = stdout
        .lines()
        .filter(|line| !line.is_empty() && !line.contains('/'))
        .map(|line| line.to_string())
        .collect();
    files.sort();
    files.dedup();
    Ok(files)
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

struct GitWorktree {
    path: PathBuf,
    branch: String,
}

pub struct SymlinkRemovalReport {
    pub removed: Vec<(String, PathBuf)>,
    pub failed: Vec<(String, PathBuf, String)>,
}

#[derive(Default)]
pub struct LinkReport {
    pub linked: Vec<(String, PathBuf)>,
    pub failed: Vec<(String, PathBuf, String)>,
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
    if !trees_dir.exists() {
        return Ok(Vec::new());
    }

    let worktrees = list_git_worktrees(project_root)?;
    let mut managed = Vec::new();

    for wt in worktrees {
        if wt.path.starts_with(&trees_dir) {
            let name = wt
                .path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            managed.push(Worktree {
                name,
                path: wt.path,
                branch: wt.branch,
            });
        }
    }

    Ok(managed)
}

pub struct ImportMove {
    pub from: PathBuf,
    pub to: PathBuf,
    pub relink_error: Option<String>,
}

pub struct ImportSkip {
    pub path: PathBuf,
    pub reason: String,
}

pub struct ImportFailure {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Default)]
pub struct ImportReport {
    pub moved: Vec<ImportMove>,
    pub skipped: Vec<ImportSkip>,
    pub failed: Vec<ImportFailure>,
}

pub fn import_all_worktrees(project_root: &Path, config: &Config) -> Result<ImportReport> {
    let trees_dir = get_trees_dir(project_root);
    fs::create_dir_all(&trees_dir)
        .with_context(|| format!("Failed to create trees dir: {}", trees_dir.display()))?;

    let worktrees = list_git_worktrees(project_root)?;
    let mut report = ImportReport::default();

    for wt in worktrees {
        if wt.path == project_root {
            report.skipped.push(ImportSkip {
                path: wt.path,
                reason: "main worktree".to_string(),
            });
            continue;
        }
        if wt.path.starts_with(&trees_dir) {
            report.skipped.push(ImportSkip {
                path: wt.path,
                reason: "already managed".to_string(),
            });
            continue;
        }

        let src_path = wt.path;
        let base_name = src_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "worktree".to_string());
        let dest = unique_import_path(&trees_dir, &base_name);
        let src_str = src_path.to_string_lossy().to_string();
        let dest_str = dest.to_string_lossy().to_string();

        let output = Command::new("git")
            .args(["worktree", "move", &src_str, &dest_str])
            .current_dir(project_root)
            .output()
            .context("Failed to run git worktree move")?;

        if !output.status.success() {
            report.failed.push(ImportFailure {
                path: src_path,
                error: format!(
                    "git worktree move failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
            continue;
        }

        let name = dest
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let relink_error = if name.is_empty() {
            Some("relink failed: unable to determine worktree name".to_string())
        } else {
            relink_worktree(project_root, &name, config)
                .err()
                .map(|err| format!("relink failed: {}", err))
        };

        report.moved.push(ImportMove {
            from: src_path,
            to: dest,
            relink_error,
        });
    }

    Ok(report)
}

pub fn remove_symlinks_from_worktrees(
    project_root: &Path,
    rel_path: &str,
) -> Result<SymlinkRemovalReport> {
    let worktrees = list_worktrees(project_root)?;
    let mut removed = Vec::new();
    let mut failed = Vec::new();

    for worktree in worktrees {
        let Worktree { name, path, .. } = worktree;
        let dst = path.join(rel_path);

        match dst.symlink_metadata() {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    if let Err(err) = fs::remove_file(&dst) {
                        failed.push((name, dst, err.to_string()));
                    } else {
                        removed.push((name, dst));
                    }
                }
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => failed.push((name, dst, err.to_string())),
        }
    }

    Ok(SymlinkRemovalReport { removed, failed })
}

pub fn link_entries_to_worktrees(
    project_root: &Path,
    entries: &[FileEntry],
) -> Result<LinkReport> {
    let worktrees = list_worktrees(project_root)?;
    let mut report = LinkReport::default();

    if worktrees.is_empty() || entries.is_empty() {
        return Ok(report);
    }

    for entry in entries {
        let src = project_root.join(&entry.path);
        if !src.exists() {
            eprintln!(
                "Warning: source file does not exist: {}",
                src.display()
            );
            continue;
        }

        for worktree in &worktrees {
            let dst = worktree.path.join(&entry.path);
            match link_entry(&src, &dst, &entry.link_type) {
                Ok(()) => report
                    .linked
                    .push((worktree.name.clone(), dst)),
                Err(err) => report.failed.push((
                    worktree.name.clone(),
                    dst,
                    err.to_string(),
                )),
            }
        }
    }

    Ok(report)
}

pub fn select_worktree_name(project_root: &Path) -> Result<Option<String>> {
    let worktrees = list_worktrees(project_root)?;
    if worktrees.is_empty() {
        anyhow::bail!("No worktrees found.");
    }
    if worktrees.len() == 1 {
        return Ok(Some(worktrees[0].name.clone()));
    }

    let names: Vec<String> = worktrees.into_iter().map(|wt| wt.name).collect();
    let selection = Select::new("Select worktree", names).prompt();
    match selection {
        Ok(name) => Ok(Some(name)),
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

        link_entry(&src, &dst, &entry.link_type)?;
    }

    Ok(())
}

fn link_entry(src: &Path, dst: &Path, link_type: &LinkType) -> Result<()> {
    // Create parent directories for destination
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent dir: {}", parent.display()))?;
    }

    // Remove existing destination if it exists
    if dst.exists() || dst.symlink_metadata().is_ok() {
        if dst.is_dir() && !dst.symlink_metadata()?.file_type().is_symlink() {
            fs::remove_dir_all(dst)?;
        } else {
            fs::remove_file(dst)?;
        }
    }

    match link_type {
        LinkType::Symlink => {
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(src, dst).with_context(|| {
                    format!("Failed to symlink {} -> {}", src.display(), dst.display())
                })?;
            }
            #[cfg(windows)]
            {
                if src.is_dir() {
                    std::os::windows::fs::symlink_dir(src, dst)?;
                } else {
                    std::os::windows::fs::symlink_file(src, dst)?;
                }
            }
        }
        LinkType::Copy => {
            if src.is_dir() {
                copy_dir_recursive(src, dst)?;
            } else {
                fs::copy(src, dst).with_context(|| {
                    format!("Failed to copy {} -> {}", src.display(), dst.display())
                })?;
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

fn list_git_worktrees(project_root: &Path) -> Result<Vec<GitWorktree>> {
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
    let mut worktrees = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_branch: Option<String> = None;

    for line in stdout.lines() {
        if line.starts_with("worktree ") {
            if let Some(path) = current_path.take() {
                worktrees.push(GitWorktree {
                    path,
                    branch: current_branch.take().unwrap_or_default(),
                });
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

    if let Some(path) = current_path {
        worktrees.push(GitWorktree {
            path,
            branch: current_branch.unwrap_or_default(),
        });
    }

    Ok(worktrees)
}

fn unique_import_path(trees_dir: &Path, base_name: &str) -> PathBuf {
    let base = if base_name.is_empty() {
        "worktree"
    } else {
        base_name
    };

    let mut candidate = trees_dir.join(base);
    if !candidate.exists() {
        return candidate;
    }

    let mut index = 2;
    loop {
        candidate = trees_dir.join(format!("{}-{}", base, index));
        if !candidate.exists() {
            return candidate;
        }
        index += 1;
    }
}
