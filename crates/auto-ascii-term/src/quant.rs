//! Color quantization for the non-truecolor tiers: the painter's
//! quantize-before-diff step. The math lives in [`auto_ascii_core::quant`],
//! where the `ascii` codec also uses it to hold its background shade to its
//! contrast cap after 256-color quantization.

pub use auto_ascii_core::quant::{ansi16_to_rgb, ansi256_to_rgb, rgb_to_16, rgb_to_256};
