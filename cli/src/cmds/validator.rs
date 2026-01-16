use std::{error::Error, path::PathBuf};

use clap::Args;
use kzgrs_backend::{
    common::share::DaShare, dispersal::Index, reconstruction::reconstruct_without_missing_data,
};

fn parse_app_shares(val: &str) -> Result<Vec<(Index, Vec<DaShare>)>, String> {
    let val: String = val.chars().filter(|&c| c != ' ' && c != '\n').collect();
    serde_json::from_str(&val).map_err(|e| e.to_string())
}

#[derive(Args, Debug)]
pub struct Reconstruct {
    /// `DaBlobs` to use for reconstruction. Half of the blobs per index is
    /// expected.
    #[clap(
        short,
        long,
        help = "JSON array of blobs [[[index0], [{DaBlob0}, {DaBlob1}...]]...]",
        value_name = "APP_BLOBS",
        value_parser(parse_app_shares)
    )]
    pub app_shares: Option<Vec<(Index, Vec<DaShare>)>>,
    /// File with blobs.
    #[clap(short, long)]
    pub file: Option<PathBuf>,
}

impl Reconstruct {
    pub fn run(self) -> Result<(), Box<dyn Error>> {
        let app_shares: Vec<(Index, Vec<DaShare>)> = if let Some(shares) = self.app_shares {
            shares
        } else {
            let file_path = self.file.as_ref().unwrap();
            let json_string = String::from_utf8(std::fs::read(file_path)?)?;
            parse_app_shares(&json_string)?
        };

        let mut da_shares = vec![];

        for (index, shares) in &app_shares {
            tracing::info!("Index {:?} has {:} shares", (index), shares.len());
            for share in shares {
                da_shares.push(share.clone());
                tracing::info!("Index {:?}; DaShare: {share:?}", index.to_u64());
            }
        }

        let reconstructed_data = reconstruct_without_missing_data(&da_shares);
        tracing::info!("Reconstructed data {:?}", reconstructed_data);

        Ok(())
    }
}
