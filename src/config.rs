use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

pub const CONFIG_DIR: &str = ".epi";
pub const CONFIG_FILE: &str = "config.toml";
pub const TREES_DIR: &str = "trees";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkType {
    Copy,
    Symlink,
}

impl Default for LinkType {
    fn default() -> Self {
        LinkType::Symlink
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    #[serde(default)]
    pub link_type: LinkType,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_main_branch")]
    pub main_branch: String,
    #[serde(default)]
    pub files: Vec<FileEntry>,
}

fn default_main_branch() -> String {
    "main".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            main_branch: default_main_branch(),
            files: Vec::new(),
        }
    }
}

impl Config {
    pub fn load(project_root: &Path) -> Result<Self> {
        let config_path = project_root.join(CONFIG_DIR).join(CONFIG_FILE);
        if !config_path.exists() {
            return Ok(Config::default());
        }
        let content = fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read config file: {}", config_path.display()))?;
        let config: Config = toml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", config_path.display()))?;
        Ok(config)
    }

    pub fn save(&self, project_root: &Path) -> Result<()> {
        let config_dir = project_root.join(CONFIG_DIR);
        fs::create_dir_all(&config_dir)
            .with_context(|| format!("Failed to create config dir: {}", config_dir.display()))?;
        let config_path = config_dir.join(CONFIG_FILE);
        let content = toml::to_string_pretty(self)
            .context("Failed to serialize config")?;
        fs::write(&config_path, content)
            .with_context(|| format!("Failed to write config file: {}", config_path.display()))?;
        Ok(())
    }
}

pub fn get_trees_dir(project_root: &Path) -> PathBuf {
    project_root.join(CONFIG_DIR).join(TREES_DIR)
}

pub fn find_project_root() -> Result<PathBuf> {
    let current_dir = std::env::current_dir().context("Failed to get current directory")?;
    let mut dir = current_dir.as_path();

    loop {
        let git_path = dir.join(".git");
        if git_path.exists() {
            // Check if .git is a file (worktree) or directory (main repo)
            if git_path.is_file() {
                // Worktree: .git file contains "gitdir: /path/to/.git/worktrees/name"
                let content = fs::read_to_string(&git_path)
                    .with_context(|| format!("Failed to read .git file: {}", git_path.display()))?;
                if let Some(gitdir) = content.strip_prefix("gitdir: ") {
                    let gitdir = gitdir.trim();
                    // gitdir points to .git/worktrees/<name>, we need the main .git
                    let gitdir_path = PathBuf::from(gitdir);
                    // Go up from .git/worktrees/<name> to .git, then to repo root
                    if let Some(git_main) = gitdir_path.parent().and_then(|p| p.parent()) {
                        if let Some(repo_root) = git_main.parent() {
                            return Ok(repo_root.to_path_buf());
                        }
                    }
                }
                // Fallback: return current dir if we can't parse the gitdir
                return Ok(dir.to_path_buf());
            } else {
                // Main repo: .git is a directory
                return Ok(dir.to_path_buf());
            }
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => anyhow::bail!("Not in a git repository"),
        }
    }
}
