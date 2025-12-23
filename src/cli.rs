use clap::Parser;
use std::path::PathBuf;

/// Check for outdated Python dependencies
#[derive(Parser, Debug, Clone)]
#[command(name = "python-check-updates")]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Path to project directory (defaults to current directory)
    #[arg(value_name = "PATH")]
    pub path: Option<PathBuf>,

    /// Apply updates to dependency files
    #[arg(short, long)]
    pub update: bool,

    /// Upgrade pinned versions to latest minor (same major version)
    #[arg(short, long)]
    pub minor: bool,

    /// Upgrade all dependencies to absolute latest (ignore version constraints)
    #[arg(short, long)]
    pub force_latest: bool,

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
