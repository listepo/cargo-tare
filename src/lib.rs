//! dunnage: shrink Cargo target directories without slowing builds. See `DESIGN.md`.

pub mod advise;
pub mod cargo_home;
pub mod compress;
pub mod config;
pub mod dedupe;
pub mod doc;
pub mod engine;
pub mod evict;
pub mod incremental;
pub mod index;
pub mod inventory;
pub mod model;
pub mod orphans;
pub mod seed;
pub mod sys;
pub mod toolchains;
