//! cargo-tare: shrink Cargo target directories without slowing builds. See `DESIGN.md`.

pub mod compress;
pub mod dedupe;
pub mod engine;
pub mod evict;
pub mod index;
pub mod inventory;
pub mod model;
pub mod orphans;
