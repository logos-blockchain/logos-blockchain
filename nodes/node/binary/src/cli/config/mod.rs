pub mod init;
pub mod keystore;
pub mod migrate;
pub mod update;

use std::io::Write as _;

pub fn confirm_overwrite(msg: &str) -> Result<bool, std::io::Error> {
    print!("{msg} (y/N): ");
    std::io::stdout().flush()?;

    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;

    let trimmed = input.trim().to_lowercase();
    Ok(trimmed == "y" || trimmed == "yes")
}
