mod config;
mod worktree;

use anyhow::Result;
use clap::{Parser, Subcommand};

use config::{find_project_root, Config, FileEntry, LinkType};
use worktree::{
    add_worktree, ensure_on_main_branch, enter_worktree, get_worktree_path, list_worktrees,
    relink_worktree, remove_worktree, resolve_worktree_name,
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

    /// Remove a worktree
    Remove {
        /// Name of the worktree to remove (auto-detected if inside a worktree)
        name: Option<String>,

        /// Force removal even with uncommitted changes
        #[arg(short, long)]
        force: bool,
    },

    /// List all worktrees managed by epiphyte
    List,

    /// Re-link/copy files from config to an existing worktree
    Relink {
        /// Name of the worktree to relink (auto-detected if inside a worktree)
        name: Option<String>,
    },

    /// Enter a worktree in a new shell
    Enter {
        /// Name of the worktree to enter (auto-detected if inside a worktree)
        name: Option<String>,
    },

    /// Manage files in the configuration
    #[command(subcommand)]
    Files(FilesCommands),
}

#[derive(Subcommand)]
enum FilesCommands {
    /// Add a file to the configuration
    Add {
        /// Path to the file (relative to project root)
        path: String,

        /// Copy the file instead of symlinking
        #[arg(short, long)]
        copy: bool,
    },

    /// Remove a file from the configuration
    Remove {
        /// Path to the file to remove
        path: String,
    },

    /// List files in the configuration
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

        Commands::Remove { name, force } => {
            let name = resolve_worktree_name(&project_root, name.as_deref())?;
            remove_worktree(&project_root, &name, force)?;
            println!("Removed worktree '{}'", name);
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
            let name = resolve_worktree_name(&project_root, name.as_deref())?;
            let path = get_worktree_path(&project_root, &name)?;
            println!("Entering worktree '{}' at {}", name, path.display());
            enter_worktree(&path)?;
        }

        Commands::Files(files_cmd) => {
            let mut config = Config::load(&project_root)?;

            match files_cmd {
                FilesCommands::Add { path, copy } => {
                    // Check if already exists
                    if config.files.iter().any(|f| f.path == path) {
                        anyhow::bail!("File '{}' is already in the configuration", path);
                    }

                    config.files.push(FileEntry {
                        path: path.clone(),
                        link_type: if copy {
                            LinkType::Copy
                        } else {
                            LinkType::Symlink
                        },
                    });
                    config.save(&project_root)?;
                    println!("Added '{}' to configuration", path);
                }

                FilesCommands::Remove { path } => {
                    let initial_len = config.files.len();
                    config.files.retain(|f| f.path != path);
                    if config.files.len() == initial_len {
                        anyhow::bail!("File '{}' not found in configuration", path);
                    }
                    config.save(&project_root)?;
                    println!("Removed '{}' from configuration", path);
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
