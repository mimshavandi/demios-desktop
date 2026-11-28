//! What the library hands the launcher (`dn-sandbox-launch`): one JSON argument. The launcher
//! applies it to itself and then `exec`s the runtime, so the runtime starts already confined
//! and the app never needs code between `fork` and `exec` (no `unsafe`).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The exit code the launcher uses when it refuses to start a run; its stderr then starts
/// with [`REFUSED_PREFIX`].
pub const REFUSED_EXIT: i32 = 125;
pub const REFUSED_PREFIX: &str = "dn-sandbox-launch: refused: ";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LaunchSpec {
    /// The runtime's executable (an absolute path).
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// The whole environment of the runtime.
    pub env: Vec<(String, String)>,
    /// `RLIMIT_FSIZE`: no single file can grow past this.
    pub file_size_cap: u64,
    /// macOS: the Seatbelt profile `sandbox-exec` loads.
    pub profile: Option<PathBuf>,
    /// Linux: folders and files the run may read (no execute right).
    pub reads: Vec<PathBuf>,
    /// Linux: folders the run may write (and read).
    pub writes: Vec<PathBuf>,
    /// Linux: files the run may execute: the runtime and the dynamic loader.
    pub execs: Vec<PathBuf>,
    /// Linux: device files the run may read and write (`/dev/null`).
    pub devices: Vec<PathBuf>,
}
