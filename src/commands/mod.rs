use anyhow::Result;

use crate::context::Context;
use crate::errors::BatchFailure;

pub mod account;
pub mod admin;
pub mod albums;
pub mod assets;
pub mod download;
pub mod people;
pub mod session;
pub mod share;
pub mod upload;

/// How a batch ends, for the three commands that run one.
///
/// A person has already had the per-item warnings and the summary line by the time this
/// is called. What is left is stdout and `$?`, and the two are decided together: in
/// `--json` exactly one document reaches stdout — `summary` when nothing was refused, and
/// otherwise that same summary carried out through `main` by `BatchFailure`. Never both.
pub fn finish_batch(
    ctx: &Context,
    summary: serde_json::Value,
    failed: usize,
    succeeded: usize,
    noun: &str,
) -> Result<()> {
    if failed == 0 {
        if ctx.out.is_json() {
            ctx.out.json(&summary)?;
        }
        return Ok(());
    }
    Err(BatchFailure::new(failed, succeeded, noun, summary).into())
}
