//! `probe jpeg` (PLAN P0-4): not implemented yet. A later task replaces this stub; only this file
//! and its tests change.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use super::{Ctx, Envelope, Output};

pub const NAME: &str = "jpeg";

/// The probe's own part of the result file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Data {}

pub fn run(_ctx: &Ctx<'_>) -> Result<Output<Data>> {
    bail!("probe {NAME} is not implemented yet (PLAN P0-4)")
}

/// The table, as a pure function of the parsed JSON.
pub fn render(e: &Envelope<Data>) -> String {
    super::md_header(e)
}

/// Consistency rules of `data`, each as `<json pointer>: <message>`.
pub fn check(_e: &Envelope<Data>) -> Vec<String> {
    Vec::new()
}
