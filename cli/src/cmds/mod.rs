use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum Command {}

impl Command {
    pub fn run(self) -> Result<(), Box<dyn std::error::Error>> {
        match self {}
    }
}
