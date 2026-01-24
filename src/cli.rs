use clap::Parser;
use std::path::PathBuf;

/// Check for outdated Python dependencies
#[derive(Parser, Debug, Clone)]
#[command(name = "python-check-updates")]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Path to project directory (defaults to current directory)
    #[arg(value_name = "PATH", conflicts_with = "global")]
    pub path: Option<PathBuf>,

    /// Check globally installed packages (uv tools, pipx, pip --user)
    #[arg(short, long)]
    pub global: bool,

    /// Update dependency files (patch updates only by default)
    #[arg(short, long)]
    pub update: bool,

    /// Include minor updates (use with -u as -um)
    #[arg(short, long)]
    pub minor: bool,

    /// Force update all to absolute latest (use with -u as -uf)
    #[arg(short, long)]
    pub force: bool,

    /// Include pre-release versions
    #[arg(short, long)]
    pub pre_release: bool,
}

impl Args {
    /// Get the project path, defaulting to current directory
    pub fn project_path(&self) -> PathBuf {
        self.path.clone().unwrap_or_else(|| PathBuf::from("."))
    }
}
