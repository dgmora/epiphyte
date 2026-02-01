mod config;
mod worktree;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use inquire::{error::InquireError, MultiSelect};
use std::path::Path;

use config::{find_project_root, Config, FileEntry, LinkType};
use worktree::{
    add_worktree, detect_current_worktree, ensure_on_main_branch, enter_worktree,
    get_worktree_path, is_path_tracked, link_entries_to_worktrees, list_ignored_files,
    list_worktrees, relink_worktree, remove_symlinks_from_worktrees, resolve_worktree_name,
    select_worktree_name,
};

#[derive(Parser)]
#[command(name = "epiphyte")]
#[command(about = "A git worktree management tool", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize epiphyte configuration in the current repository
    Init,

    /// Add a new worktree
    Add {
        /// Name for the worktree (also used as branch name if no branch specified)
        name: String,

        /// Existing branch to checkout (creates new branch if not specified)
        #[arg(short, long)]
        branch: Option<String>,

        /// Enter the worktree in a new shell after creation
        #[arg(short, long)]
        enter: bool,
    },

    /// List all worktrees managed by epiphyte
    #[command(visible_alias = "ls")]
    List,

    /// Re-link/copy files from config to an existing worktree
    Relink {
        /// Name of the worktree to relink (auto-detected if inside a worktree)
        name: Option<String>,
    },

    /// Enter a worktree in a new shell
    #[command(visible_alias = "e")]
    Enter {
        /// Name of the worktree to enter (auto-detected if inside a worktree)
        name: Option<String>,
    },

    /// Enter the repository root in a new shell
    Root,

    /// Manage files in the configuration
    #[command(subcommand)]
    Files(FilesCommands),
}

#[derive(Subcommand)]
enum FilesCommands {
    /// Add a file to the configuration
    Add {
        /// Path to the file (relative to project root)
        path: Option<String>,

        /// Copy the file instead of symlinking
        #[arg(short, long)]
        copy: bool,

        /// Add ignored files from the repository root (prompted)
        #[arg(long)]
        ignored: bool,
    },

    /// Remove a file from the configuration
    #[command(visible_alias = "rm")]
    Remove {
        /// Path to the file to remove
        path: String,
    },

    /// List files in the configuration
    #[command(visible_alias = "ls")]
    List,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let project_root = find_project_root()?;

    match cli.command {
        Commands::Init => {
            let config = Config::default();
            config.save(&project_root)?;
            println!(
                "Initialized epiphyte configuration at {}/.epi/config.toml",
                project_root.display()
            );
        }

        Commands::Add {
            name,
            branch,
            enter,
        } => {
            let config = Config::load(&project_root)?;
            ensure_on_main_branch(&project_root, &config.main_branch)?;
            let path = add_worktree(&project_root, &name, branch.as_deref(), &config)?;
            println!("Created worktree '{}' at {}", name, path.display());
            if enter {
                println!("Entering worktree...");
                enter_worktree(&path)?;
            }
        }

        Commands::List => {
            let worktrees = list_worktrees(&project_root)?;
            if worktrees.is_empty() {
                println!("No worktrees found");
            } else {
                for wt in worktrees {
                    println!("{}\t{}\t{}", wt.name, wt.branch, wt.path.display());
                }
            }
        }

        Commands::Relink { name } => {
            let name = resolve_worktree_name(&project_root, name.as_deref())?;
            let config = Config::load(&project_root)?;
            relink_worktree(&project_root, &name, &config)?;
            println!("Re-linked files for worktree '{}'", name);
        }

        Commands::Enter { name } => {
            let name = match name {
                Some(name) => name,
                None => match select_worktree_name(&project_root)? {
                    Some(name) => name,
                    None => return Ok(()),
                },
            };
            if detect_current_worktree(&project_root)?.as_deref() == Some(name.as_str()) {
                return Ok(());
            }
            let path = get_worktree_path(&project_root, &name)?;
            println!("Entering worktree '{}' at {}", name, path.display());
            enter_worktree(&path)?;
        }

        Commands::Root => {
            let current_dir =
                std::env::current_dir().context("Failed to get current directory")?;
            if current_dir == project_root {
                return Ok(());
            }
            println!("Entering repo root at {}", project_root.display());
            enter_worktree(&project_root)?;
        }

        Commands::Files(files_cmd) => {
            let mut config = Config::load(&project_root)?;

            match files_cmd {
                FilesCommands::Add {
                    path,
                    copy,
                    ignored,
                } => {
                    let link_type = if copy {
                        LinkType::Copy
                    } else {
                        LinkType::Symlink
                    };

                    let paths = match (ignored, path) {
                        (true, Some(_)) => {
                            anyhow::bail!("--ignored cannot be used with a path")
                        }
                        (true, None) => select_ignored_files(&project_root, &config)?,
                        (false, Some(path)) => {
                            if config.files.iter().any(|f| f.path == path) {
                                anyhow::bail!(
                                    "File '{}' is already in the configuration",
                                    path
                                );
                            }
                            if is_path_tracked(&project_root, &path)? {
                                anyhow::bail!(
                                    "File '{}' is tracked by git; only untracked files can be added",
                                    path
                                );
                            }
                            vec![path]
                        }
                        (false, None) => {
                            anyhow::bail!(
                                "Path is required unless --ignored is used"
                            )
                        }
                    };

                    if paths.is_empty() {
                        return Ok(());
                    }

                    let new_entries: Vec<FileEntry> = paths
                        .into_iter()
                        .map(|path| FileEntry {
                            path,
                            link_type: link_type.clone(),
                        })
                        .collect();
                    let count = new_entries.len();
                    let single_path = new_entries.first().map(|entry| entry.path.clone());

                    config.files.extend(new_entries.clone());
                    config.save(&project_root)?;
                    if count == 1 {
                        println!(
                            "Added '{}' to configuration",
                            single_path.unwrap()
                        );
                    } else {
                        println!("Added {} file(s) to configuration", count);
                    }

                    let report =
                        link_entries_to_worktrees(&project_root, &new_entries)?;
                    if report.linked.is_empty() {
                        println!("No worktrees updated");
                    } else {
                        println!("Linked files to worktrees:");
                        for (name, linked_path) in report.linked {
                            println!("{}\t{}", name, linked_path.display());
                        }
                    }
                    if !report.failed.is_empty() {
                        eprintln!("Warning: failed to link some files:");
                        for (name, failed_path, error) in report.failed {
                            eprintln!(
                                "{}\t{}\t{}",
                                name,
                                failed_path.display(),
                                error
                            );
                        }
                    }
                }

                FilesCommands::Remove { path } => {
                    let initial_len = config.files.len();
                    config.files.retain(|f| f.path != path);
                    if config.files.len() == initial_len {
                        anyhow::bail!("File '{}' not found in configuration", path);
                    }
                    config.save(&project_root)?;
                    println!("Removed '{}' from configuration", path);

                    let report = remove_symlinks_from_worktrees(&project_root, &path)?;
                    if report.removed.is_empty() {
                        println!("No symlinks removed from worktrees");
                    } else {
                        println!("Removed symlinks from worktrees:");
                        for (name, removed_path) in report.removed {
                            println!("{}\t{}", name, removed_path.display());
                        }
                    }
                    if !report.failed.is_empty() {
                        eprintln!("Warning: failed to remove some symlinks:");
                        for (name, failed_path, error) in report.failed {
                            eprintln!("{}\t{}\t{}", name, failed_path.display(), error);
                        }
                    }
                }

                FilesCommands::List => {
                    if config.files.is_empty() {
                        println!("No files configured");
                    } else {
                        for entry in &config.files {
                            let link_type = match entry.link_type {
                                LinkType::Copy => "copy",
                                LinkType::Symlink => "symlink",
                            };
                            println!("{}\t[{}]", entry.path, link_type);
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

fn select_ignored_files(
    project_root: &Path,
    config: &Config,
) -> Result<Vec<String>> {
    let candidates: Vec<String> = list_ignored_files(project_root)?
        .into_iter()
        .filter(|p| !config.files.iter().any(|f| f.path == *p))
        .collect();

    if candidates.is_empty() {
        println!("No ignored files found to add");
        return Ok(Vec::new());
    }

    let selection = MultiSelect::new("Select root ignored files to add", candidates).prompt();

    let selected = match selection {
        Ok(files) => files,
        Err(InquireError::OperationCanceled)
        | Err(InquireError::OperationInterrupted) => {
            return Ok(Vec::new());
        }
        Err(err) => {
            return Err(err)
                .context("Failed to prompt for ignored file selection")
        }
    };

    if selected.is_empty() {
        println!("No files selected");
        return Ok(Vec::new());
    }

    Ok(selected)
}
