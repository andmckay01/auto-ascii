//! ASCI META provenance and palette-hint payload.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub factory_version: String,
    pub source: String,
    #[serde(default)]
    pub palette_hints: Vec<String>,
}
