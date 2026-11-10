pub mod completions;
pub mod composition;
pub mod config;
pub mod function;
pub mod invoke;
pub mod ping;

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};

/// Reads a file, or stdin when `arg` is `-`.
pub fn read_text_arg(arg: &str) -> Result<String> {
    if arg == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s).context("cannot read stdin")?;
        Ok(s)
    } else {
        read_text(Path::new(arg))
    }
}

pub fn read_text(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))
}

pub fn read_bytes(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))
}

/// Prints the server's plain-text success message, e.g. `Function registered`.
pub fn print_message(body: &[u8]) {
    let text = String::from_utf8_lossy(body);
    let text = text.trim_end();
    if !text.is_empty() {
        println!("{text}");
    }
}
