use anyhow::Result;

use crate::cli::{CompositionCmd, Globals};
use crate::client::{Client, composition_document};
use crate::config::Settings;

pub const REGISTER_PATH: &str = "/register/composition";

pub fn run(globals: &Globals, cmd: CompositionCmd) -> Result<()> {
    match cmd {
        CompositionCmd::Register { file } => {
            let text = super::read_text_arg(&file)?;
            let settings = Settings::load(globals)?;
            let client = Client::new(&settings)?;
            let body = client.post_bson(REGISTER_PATH, &composition_document(&text))?;
            super::print_message(&body);
            Ok(())
        }
    }
}
