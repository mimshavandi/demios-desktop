//! `dn-models` — everything about model *files* (ADR-0003): reading GGUF metadata
//! ([`gguf`]), deciding whether a model fits this machine ([`fit`], formula in its docs),
//! the curated catalogue ([`catalog`], tested recommendations), the Hugging Face Hub client
//! ([`hub`], ADR-0011) and resumable, SHA-256-verified downloads ([`download`]).
//!
//! No llama.cpp dependency: the fit estimator works from the GGUF header alone, so it runs
//! in CI and in the UI before a model is ever loaded.

pub mod cache;
pub mod catalog;
pub mod compute;
pub mod download;
pub mod fit;
pub mod gguf;
pub mod hub;
pub mod kind;
pub mod link;
pub mod template;
#[cfg(test)]
mod testserver;

pub use catalog::{Catalog, CatalogEntry, Recommendation};
pub use fit::{FitReport, Machine, MemoryKind, ModelFootprint, fit};
pub use gguf::Gguf;

/// Total physical RAM of this machine, in bytes.
pub fn total_ram() -> u64 {
    let mut s = sysinfo::System::new();
    s.refresh_memory();
    s.total_memory()
}

/// Memory layout guess without GPU probing: unified on Apple Silicon, CPU-only otherwise.
/// Callers with a GPU device list (e.g. `dn-inference`) refine it.
pub fn detect_machine() -> Machine {
    let ram = total_ram();
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Machine::unified(ram)
    } else {
        Machine::cpu_only(ram)
    }
}
