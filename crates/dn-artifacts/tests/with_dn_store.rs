//! The artifact migrations run under dn-store's runner, the way the app opens its database.

use dn_artifacts::{ArtifactStore, OpContext, migrations};
use dn_core::Id;
use dn_core::artifact::{ArtifactOp, ArtifactType};
use dn_store::{MigrationSet, run_migrations};

#[test]
fn migrations_run_under_dn_store_and_are_idempotent() {
    let sets = [MigrationSet::new("artifact", migrations())];
    let mut conn = dn_store::open_in_memory(&sets).unwrap();
    // Running again is a no-op.
    run_migrations(&mut conn, &sets).unwrap();

    let store = ArtifactStore::new(&conn);
    let conv = Id::new();
    let v1 = store
        .apply(
            conv,
            &ArtifactOp::Create {
                artifact_type: ArtifactType::Markdown,
                title: "Notes".into(),
                content: "# One".into(),
            },
            &OpContext::model(None),
        )
        .unwrap();
    let id = v1.artifact_id;
    store
        .apply(
            conv,
            &ArtifactOp::Update {
                id,
                old_str: "One".into(),
                new_str: "Two".into(),
            },
            &OpContext::model(None),
        )
        .unwrap();
    let rb = store.rollback(conv, id, 1, &OpContext::user()).unwrap();
    assert_eq!((rb.version.n, rb.version.parent_n), (3, Some(1)));
    assert_eq!(store.latest(id).unwrap().content, "# One");
}
