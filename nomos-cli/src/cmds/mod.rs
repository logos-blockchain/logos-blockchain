pub mod validator;

use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum Command {
    Reconstruct(validator::Reconstruct),
}

impl Command {
    pub fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        match self {
            Self::Reconstruct(cmd) => cmd.run(),
        }?;
        Ok(())
    }
}
