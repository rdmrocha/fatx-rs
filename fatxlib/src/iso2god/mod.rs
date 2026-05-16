//! ISO → Games-on-Demand conversion pipeline.
//!
//! Vendored from [QAston/iso2god-rs `xdvdfx` branch](https://github.com/QAston/iso2god-rs/tree/xdvdfx)
//! (parent: [iliazeus/iso2god-rs](https://github.com/iliazeus/iso2god-rs);
//! both MIT-licensed). We keep the upstream module shape (`god/`, `executable/`)
//! so we can re-sync against new upstream commits with minimal diff. Local
//! deviations from upstream:
//!
//! - `anyhow::Error` → [`crate::error::FatxError`] so errors flow through
//!   the same channel as the rest of fatxlib.
//! - Intra-crate `use crate::god` / `use crate::executable` imports rewritten
//!   to `use crate::iso2god::god` / `use crate::iso2god::executable`.
//! - The original `src/game_list/` (4.9 KLOC of compiled-in title catalog) is
//!   dropped; fatxlib already has a richer catalog via [`crate::titles`].
//! - The upstream binary (`src/bin/iso2god.rs`) lives elsewhere — fatxlib only
//!   provides the library surface; the CLI/TUI wraps it in `xtafkit`.
//!
//! See `NOTICE` at the repo root for the full attribution.

pub mod executable;
pub mod god;

mod convert;
pub use convert::{ConvertOptions, ConvertReport, SOURCE_BUFFER_SIZE, TrimMode, convert_iso};
