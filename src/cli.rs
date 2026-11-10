use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};

use crate::parse;

#[derive(Parser, Debug)]
#[command(name = "dandelion", version, about = "Command-line client for the Dandelion runtime API")]
pub struct Cli {
    /// Server URL. Overrides DANDELION_URL and the config.
    #[arg(long, global = true, value_name = "URL")]
    pub url: Option<String>,

    /// Config profile to use. Overrides DANDELION_PROFILE.
    #[arg(long, global = true, value_name = "NAME")]
    pub profile: Option<String>,

    /// Request timeout, e.g. `30s` or `5m`.
    #[arg(long, global = true, value_name = "DURATION", value_parser = parse::duration)]
    pub timeout: Option<Duration>,

    /// Print the request path, status and timings to stderr.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    #[command(subcommand)]
    pub command: Command,
}

/// Global options, separated from the subcommand so they can be passed around.
#[derive(Debug, Clone, Default)]
pub struct Globals {
    pub url: Option<String>,
    pub profile: Option<String>,
    pub timeout: Option<Duration>,
    pub verbose: bool,
}

impl Cli {
    pub fn globals(&self) -> Globals {
        Globals {
            url: self.url.clone(),
            profile: self.profile.clone(),
            timeout: self.timeout,
            verbose: self.verbose,
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Manage functions.
    Function {
        #[command(subcommand)]
        cmd: FunctionCmd,
    },
    /// Manage compositions.
    Composition {
        #[command(subcommand)]
        cmd: CompositionCmd,
    },
    /// Invoke a registered function or composition, or an inline composition.
    Invoke(InvokeArgs),
    /// Read and change the config file.
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Check that an HTTP server is answering at the configured URL.
    Ping,
    /// Generate a shell completion script.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand, Debug)]
pub enum FunctionCmd {
    /// Register a function binary.
    Register(FunctionRegisterArgs),
}

#[derive(Args, Debug, Default)]
pub struct FunctionRegisterArgs {
    /// Function binary to upload.
    #[arg(value_name = "BINARY")]
    pub binary: Option<PathBuf>,

    /// Function name [default: file stem of BINARY].
    #[arg(long, value_name = "N")]
    pub name: Option<String>,

    /// Engine: Process, Kvm or Cheri [default: from config].
    #[arg(long, value_name = "ENGINE", value_parser = parse::engine)]
    pub engine: Option<String>,

    /// Default context size, e.g. `64M` or `1G` [default: from config].
    #[arg(long, value_name = "SIZE", value_parser = parse::size)]
    pub context_size: Option<u64>,

    /// Path to the binary on the server's disk; BINARY is then not needed.
    #[arg(long, value_name = "PATH")]
    pub server_path: Option<String>,

    /// Input set name (repeatable, in order).
    #[arg(long = "input-set", value_name = "NAME")]
    pub input_sets: Vec<String>,

    /// Static item for an input set: `<SET>:<ITEM>=@<FILE>` (repeatable).
    #[arg(long = "static", value_name = "SET:ITEM=@FILE", value_parser = parse::static_item)]
    pub statics: Vec<parse::StaticItemArg>,

    /// Output set name (repeatable, in order).
    #[arg(long = "output-set", value_name = "NAME")]
    pub output_sets: Vec<String>,

    /// Minimum set size in bytes (repeatable).
    #[arg(long = "min-set-bytes", value_name = "N", value_parser = parse::size)]
    pub min_set_bytes: Vec<u64>,

    /// Read the registration from a TOML manifest. Flags override its values.
    #[arg(long, value_name = "FILE")]
    pub manifest: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum CompositionCmd {
    /// Register composition(s) from a file, or from stdin with `-`.
    Register {
        #[arg(value_name = "FILE | -")]
        file: String,
    },
}

#[derive(Args, Debug, Default)]
pub struct InvokeArgs {
    /// Name of the registered function or composition.
    #[arg(value_name = "NAME", required_unless_present = "composition", conflicts_with = "composition")]
    pub name: Option<String>,

    /// Run an unregistered composition from a file, or from stdin with `-`.
    #[arg(long, value_name = "FILE | -")]
    pub composition: Option<String>,

    /// Input item: `<SET>:<ITEM>[#KEY]=@<FILE>` or `<SET>:<ITEM>[#KEY]=<literal>` (repeatable).
    #[arg(long = "input", value_name = "SET:ITEM[#KEY]=VALUE", value_parser = parse::input_item)]
    pub inputs: Vec<parse::InputItemArg>,

    /// Directory with one subdirectory per set and one file per item (`<ITEM>[#KEY]`).
    #[arg(long, value_name = "DIR", conflicts_with = "inputs")]
    pub input_dir: Option<PathBuf>,

    /// Use the cold (uncached) invoke path.
    #[arg(long)]
    pub no_cache: bool,

    /// Write each output item to DIR/<set>/<item>.
    #[arg(long, value_name = "DIR", group = "output_mode")]
    pub out_dir: Option<PathBuf>,

    /// Print the response as one JSON document.
    #[arg(long, group = "output_mode")]
    pub json: bool,

    /// Write only this item's bytes to stdout. SET is the set index; ITEM is the identifier or index.
    #[arg(long, value_name = "SET:ITEM", group = "output_mode")]
    pub raw: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCmd {
    /// Print one key, or all keys, of the active profile.
    Get { key: Option<String> },
    /// Set a key in the active profile.
    Set { key: String, value: String },
    /// Remove a key from the active profile.
    Unset { key: String },
    /// Print every profile and its settings.
    List,
    /// Print the config file location.
    Path,
    /// Manage profiles.
    Profile {
        #[command(subcommand)]
        cmd: ProfileCmd,
    },
}

#[derive(Subcommand, Debug)]
pub enum ProfileCmd {
    /// List profile names; the active one is marked with `*`.
    List,
    /// Add a profile. Use the global `--url` to set its URL.
    Add { name: String },
    /// Make a profile the active one.
    Use { name: String },
    /// Remove a profile.
    Remove { name: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn invoke_name_and_composition_conflict() {
        let r = Cli::try_parse_from(["dandelion", "invoke", "f", "--composition", "c.txt"]);
        assert!(r.is_err());
        let r = Cli::try_parse_from(["dandelion", "invoke"]);
        assert!(r.is_err());
    }

    #[test]
    fn global_flags_after_subcommand() {
        let cli = Cli::try_parse_from([
            "dandelion", "config", "profile", "add", "remote", "--url", "http://h:1",
        ])
        .unwrap();
        assert_eq!(cli.url.as_deref(), Some("http://h:1"));
    }

    #[test]
    fn output_modes_are_exclusive() {
        let r = Cli::try_parse_from(["dandelion", "invoke", "f", "--json", "--raw", "0:a"]);
        assert!(r.is_err());
    }
}
