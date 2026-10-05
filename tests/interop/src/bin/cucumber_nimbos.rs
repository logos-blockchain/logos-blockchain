//! Run the existing Cucumber scenarios against Nimbos.
//!
//! Set `NIMBOS_NODE_BIN` and `NIMBOS_CIRCUITS_DIR`, then run:
//! ```text
//! cargo run -p blockchain-test-interop --bin cucumber_nimbos \
//!   -- --name '^Two nodes happy path$'
//! ```
//! The suite supplies shared network inputs. Genesis and API incompatibilities
//! fail normally; typed Logos scenarios remain unsupported.

use std::process::ExitCode;

use blockchain_test_interop::nimbos;
use logos_blockchain_tests::cucumber::runner;

#[tokio::main]
async fn main() -> ExitCode {
    runner::run(nimbos::cucumber::implementation()).await
}
