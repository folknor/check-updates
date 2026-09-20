pub mod fs;
pub mod output;
pub mod resolver;
pub mod types;
pub mod version;

// Re-export commonly used types at crate root
pub use fs::{AtomicWriteError, write_atomically};
pub use output::TableRenderer;
pub use resolver::DependencyResolver;
pub use types::{Dependency, DependencyCheck, PackageInfo, UpdateSeverity};
pub use version::{Version, VersionError, VersionSpec};
