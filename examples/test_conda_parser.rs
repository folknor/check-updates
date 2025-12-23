// Example demonstrating the conda environment.yml parser
// Run with: cargo run --example test_conda_parser

use python_check_updates::parsers::{CondaParser, DependencyParser};
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let parser = CondaParser::new();
    let path = PathBuf::from("examples/environment.yml");

    println!("Parsing: {}", path.display());
    println!("Can parse: {}\n", parser.can_parse(&path));

    let dependencies = parser.parse(&path)?;

    println!("Found {} dependencies:\n", dependencies.len());

    for dep in dependencies {
        println!(
            "{:20} {:20} (line {})",
            dep.name,
            format!("{}", dep.version_spec),
            dep.line_number
        );
    }

    Ok(())
}
