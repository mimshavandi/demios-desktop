//! TypeScript bindings for the UI (ts-rs). `bindings/` is committed; this test fails when
//! it is stale. Regenerate with:
//!
//! ```sh
//! DN_UPDATE_BINDINGS=1 cargo test -p dn-core --features ts ts_bindings
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use ts_rs::TS;

fn export_all(dir: &Path) {
    // Config::new() ignores TS_RS_* env vars, so output does not depend on the shell.
    let cfg = ts_rs::Config::new().with_out_dir(dir);
    macro_rules! export {
        ($($t:ty),* $(,)?) => { $( <$t>::export_all(&cfg).expect(stringify!($t)); )* };
    }
    use crate::{artifact::*, capability::*, chat::*, error::*, event::*, id::*, model::*, run::*};
    export!(
        Id,
        ArtifactType,
        Author,
        ArtifactVersion,
        Artifact,
        ArtifactOp,
        RiskTier,
        ApprovalPolicy,
        CapabilitySource,
        ApprovalRequester,
        CapabilitySpec,
        Preflight,
        Role,
        ContentPart,
        ToolCall,
        ChatMessage,
        ToolSpec,
        Reasoning,
        ChatRequest,
        FinishReason,
        Usage,
        StopReason,
        DropAction,
        Dropped,
        RunResult,
        StreamEvent,
        RecoveryAction,
        CoreError,
        Envelope,
        Capability,
        ModelSlot,
        Readiness,
        FitStatus,
    );
}

fn read_dir(dir: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "ts") {
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                out.insert(name, std::fs::read_to_string(&p).unwrap());
            }
        }
    }
    out
}

#[test]
fn ts_bindings_are_current() {
    let committed = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bindings");
    let fresh = std::env::temp_dir().join(format!("dn-core-bindings-{}", crate::Id::new()));
    export_all(&fresh);
    let generated = read_dir(&fresh);
    let _ = std::fs::remove_dir_all(&fresh);
    assert!(generated.len() > 20, "expected the full set of bindings");

    if std::env::var_os("DN_UPDATE_BINDINGS").is_some() {
        let _ = std::fs::remove_dir_all(&committed);
        std::fs::create_dir_all(&committed).unwrap();
        for (name, body) in &generated {
            std::fs::write(committed.join(name), body).unwrap();
        }
        return;
    }
    let on_disk = read_dir(&committed);
    assert!(
        on_disk == generated,
        "crates/dn-core/bindings is stale; run: DN_UPDATE_BINDINGS=1 cargo test -p dn-core --features ts ts_bindings"
    );
}
