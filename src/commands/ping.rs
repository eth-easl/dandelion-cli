use anyhow::Result;

use crate::cli::Globals;
use crate::client::{Client, check_status};
use crate::config::Settings;
use crate::output::format_duration;

/// GETs `/`. The server answers every unknown path with 200, so this only shows that an HTTP
/// server is answering at the URL.
pub fn run(globals: &Globals) -> Result<()> {
    let settings = Settings::load(globals)?;
    let client = Client::new(&settings)?;
    let (status, body, elapsed) = client.get("/")?;
    println!("{}: {status} in {}", client.url_for("/"), format_duration(elapsed));
    check_status(status, &body)
}
