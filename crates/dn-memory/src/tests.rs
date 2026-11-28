use super::*;
use rusqlite::Connection;

fn db() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    for m in migrations() {
        c.execute_batch(m).unwrap();
    }
    c
}

fn msg(id: &str, conv: &str) -> Source {
    Source {
        kind: SourceKind::Message,
        ref_id: id.into(),
        conversation_id: Some(conv.into()),
        title: "Trip to Lisbon".into(),
        at: 1,
    }
}

fn suggestion(scope: Scope, text: &str, third_party: bool, kind: Kind) -> Suggestion {
    Suggestion {
        scope,
        kind,
        subject: "you".into(),
        text: text.into(),
        third_party,
        created_by: "model:qwen3-8b@memory-extract-v1".into(),
        sources: vec![msg("m1", "c1")],
    }
}

#[test]
fn scopes_round_trip_and_bad_ones_are_refused() {
    for s in [
        Scope::Account,
        Scope::Project("p1".into()),
        Scope::Addon("mail".into()),
    ] {
        assert_eq!(Scope::parse(&s.to_string()).unwrap(), s);
    }
    for bad in ["", "project:", "team:x", "Account"] {
        assert!(Scope::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn the_persons_own_memory_is_active_at_once_and_trusted_most() {
    let c = db();
    let it = add_user(
        &c,
        Scope::Account,
        Kind::Preference,
        "you",
        " Prefers metric units ",
        None,
    )
    .unwrap();
    assert_eq!(
        (it.status, it.trust, it.text.as_str()),
        (Status::Active, Trust::UserStated, "Prefers metric units")
    );
    assert!(add_user(&c, Scope::Account, Kind::Fact, "you", "  ", None).is_err());
    assert_eq!(list(&c, &Filter::default()).unwrap().len(), 1);
}

#[test]
fn inferred_memories_wait_unless_remembered_automatically_and_third_party_always_waits() {
    let c = db();
    let p = suggest(
        &c,
        suggestion(
            Scope::Account,
            "Likes window seats",
            false,
            Kind::Preference,
        ),
    )
    .unwrap();
    assert_eq!((p.status, p.trust), (Status::Pending, Trust::OwnContent));

    set_settings(
        &c,
        &Scope::Account,
        Settings {
            remember_automatically: true,
            ..Default::default()
        },
    )
    .unwrap();
    let a = suggest(
        &c,
        suggestion(Scope::Account, "Flies from Hamburg", false, Kind::Fact),
    )
    .unwrap();
    assert_eq!(a.status, Status::Active);
    // Someone else's content: always a pending claim, trust 0.
    let t = suggest(
        &c,
        suggestion(
            Scope::Account,
            "Alice says the review moved to Friday",
            true,
            Kind::Commitment,
        ),
    )
    .unwrap();
    assert_eq!(
        (t.status, t.trust, t.third_party),
        (Status::Pending, Trust::ThirdParty, true)
    );
    // …and never a preference, style or correction.
    for k in [
        Kind::Preference,
        Kind::Style,
        Kind::Correction,
        Kind::Project,
    ] {
        assert!(
            suggest(&c, suggestion(Scope::Account, "x", true, k)).is_err(),
            "{k:?}"
        );
    }
    // An inferred memory names its sources and model.
    let mut s = suggestion(Scope::Account, "x", false, Kind::Fact);
    s.sources.clear();
    assert!(suggest(&c, s).is_err());
    let mut s = suggestion(Scope::Account, "x", false, Kind::Fact);
    s.created_by = "user".into();
    assert!(suggest(&c, s).is_err());
}

#[test]
fn memory_off_adds_nothing_and_reads_nothing_but_the_list_stays() {
    let c = db();
    add_user(
        &c,
        Scope::Account,
        Kind::Fact,
        "you",
        "Lives in Lisbon",
        None,
    )
    .unwrap();
    set_settings(
        &c,
        &Scope::Account,
        Settings {
            enabled: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(
        add_user(&c, Scope::Account, Kind::Fact, "you", "x", None),
        Err(MemoryError::Off)
    ));
    assert!(matches!(
        suggest(&c, suggestion(Scope::Account, "x", false, Kind::Fact)),
        Err(MemoryError::Off)
    ));
    assert!(read_scopes(&c, None).unwrap().is_empty());
    assert!(read_scopes(&c, Some("p1")).unwrap().is_empty());
    assert_eq!(
        list(&c, &Filter::default()).unwrap().len(),
        1,
        "still listed and editable"
    );
}

#[test]
fn a_project_reads_the_account_only_with_use_account_on() {
    let c = db();
    assert_eq!(read_scopes(&c, None).unwrap(), [Scope::Account]);
    let p = Scope::Project("p1".into());
    assert_eq!(
        read_scopes(&c, Some("p1")).unwrap(),
        [p.clone(), Scope::Account]
    );
    set_settings(
        &c,
        &p,
        Settings {
            use_account: false,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        read_scopes(&c, Some("p1")).unwrap(),
        std::slice::from_ref(&p)
    );
    assert_eq!(write_scope(Some("p1")), p);
    assert_eq!(write_scope(None), Scope::Account);
}

#[test]
fn keep_dismiss_and_undo_move_suggestions_through_the_queue() {
    let c = db();
    let a = suggest(
        &c,
        suggestion(
            Scope::Account,
            "Likes window seats",
            false,
            Kind::Preference,
        ),
    )
    .unwrap();
    let b = suggest(
        &c,
        suggestion(Scope::Account, "Alice says hi", true, Kind::Fact),
    )
    .unwrap();
    assert_eq!(pending(&c).unwrap().len(), 2);
    let k = keep(&c, &a.id).unwrap();
    assert_eq!((k.status, k.trust), (Status::Active, Trust::UserConfirmed));
    assert!(keep(&c, &a.id).is_err(), "only a pending item is kept");
    dismiss(&c, &b.id).unwrap();
    assert!(pending(&c).unwrap().is_empty());
    undo_dismiss(&c, &b.id).unwrap();
    let kept = keep(&c, &b.id).unwrap();
    assert!(kept.third_party, "a kept claim stays a claim");
    let x = suggest(&c, suggestion(Scope::Account, "y", false, Kind::Fact)).unwrap();
    assert_eq!(dismiss_all(&c).unwrap(), [x.id]);
}

#[test]
fn an_edit_keeps_the_old_text_as_a_version_and_is_then_the_persons() {
    let c = db();
    let s = suggest(
        &c,
        suggestion(
            Scope::Project("p1".into()),
            "Alice says the review is on Friday",
            true,
            Kind::Fact,
        ),
    )
    .unwrap();
    let e = edit(
        &c,
        &s.id,
        Edit {
            text: "The review is on Friday".into(),
            kind: Kind::Commitment,
            subject: "Review".into(),
            scope: Scope::Account,
        },
    )
    .unwrap();
    assert_eq!(
        (e.status, e.trust, e.third_party, e.scope.clone()),
        (Status::Active, Trust::UserStated, false, Scope::Account)
    );
    let d = detail(&c, &s.id).unwrap().unwrap();
    assert_eq!(d.versions.len(), 1);
    assert_eq!(d.versions[0].text, "Alice says the review is on Friday");
    assert_eq!(d.versions[0].scope, Scope::Project("p1".into()));
    // An add-on's memory can't be moved out, or another's in.
    let a = add_user(&c, Scope::Addon("mail".into()), Kind::Fact, "x", "y", None).unwrap();
    assert!(
        edit(
            &c,
            &a.id,
            Edit {
                text: "y".into(),
                kind: Kind::Fact,
                subject: "x".into(),
                scope: Scope::Account
            }
        )
        .is_err()
    );
}

#[test]
fn search_filters_and_supersede() {
    let c = db();
    let a = add_user(
        &c,
        Scope::Account,
        Kind::Fact,
        "work",
        "Works at Contoso",
        None,
    )
    .unwrap();
    add_user(
        &c,
        Scope::Account,
        Kind::Preference,
        "you",
        "Prefers metric units",
        None,
    )
    .unwrap();
    let hits = list(
        &c,
        &Filter {
            query: Some("contoso".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(
        list(
            &c,
            &Filter {
                query: Some("\" OR *".into()),
                ..Default::default()
            }
        )
        .is_ok(),
        "query text can't break the FTS syntax"
    );
    assert_eq!(
        list(
            &c,
            &Filter {
                kind: Some(Kind::Preference),
                ..Default::default()
            }
        )
        .unwrap()
        .len(),
        1
    );
    let b = add_user(
        &c,
        Scope::Account,
        Kind::Fact,
        "work",
        "Works at Fabrikam",
        None,
    )
    .unwrap();
    supersede(&c, &a.id, &b.id).unwrap();
    let old = get(&c, &a.id).unwrap().unwrap();
    assert_eq!(
        (old.status, old.superseded_by.as_deref()),
        (Status::Superseded, Some(b.id.as_str()))
    );
    assert!(old.valid_to.is_some());
    assert!(supersede(&c, &a.id, &b.id).is_err(), "only active ones");
}

#[test]
fn delete_restores_with_undo_and_delete_all_keeps_addon_memories() {
    let c = db();
    let a = add_user(
        &c,
        Scope::Account,
        Kind::Fact,
        "you",
        "Lives in Lisbon",
        Some(msg("m1", "c1")),
    )
    .unwrap();
    edit(
        &c,
        &a.id,
        Edit {
            text: "Lives in Porto".into(),
            kind: Kind::Fact,
            subject: "you".into(),
            scope: Scope::Account,
        },
    )
    .unwrap();
    let d = delete(&c, &a.id).unwrap();
    assert!(get(&c, &a.id).unwrap().is_none());
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM memory_tombstones", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    restore(&c, &d).unwrap();
    let back = detail(&c, &a.id).unwrap().unwrap();
    // The chat message, and the edit that made it the person's.
    assert_eq!((back.sources.len(), back.versions.len()), (2, 1));
    assert_eq!(
        list(
            &c,
            &Filter {
                query: Some("porto".into()),
                ..Default::default()
            }
        )
        .unwrap()
        .len(),
        1,
        "searchable again"
    );
    assert_eq!(
        c.query_row("SELECT COUNT(*) FROM memory_tombstones", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );

    add_user(
        &c,
        Scope::Addon("mail".into()),
        Kind::Fact,
        "x",
        "Add-on note",
        None,
    )
    .unwrap();
    add_user(
        &c,
        Scope::Project("p1".into()),
        Kind::Fact,
        "x",
        "Project note",
        None,
    )
    .unwrap();
    assert_eq!(delete_all_memories(&c).unwrap().len(), 2);
    let left = list(&c, &Filter::default()).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].scope, Scope::Addon("mail".into()));
    assert_eq!(delete_addon(&c, "mail").unwrap(), 1);
    delete_all_local(&c).unwrap();
    assert!(export(&c).unwrap()["items"].as_array().unwrap().is_empty());
}

#[test]
fn forgetting_a_chat_deletes_only_items_left_without_a_source() {
    let c = db();
    let only = suggest(
        &c,
        suggestion(Scope::Account, "From chat 1 only", false, Kind::Fact),
    )
    .unwrap();
    let mut s = suggestion(Scope::Account, "From two chats", false, Kind::Fact);
    s.sources.push(msg("m9", "c2"));
    let both = suggest(&c, s).unwrap();
    let mine = add_user(
        &c,
        Scope::Account,
        Kind::Fact,
        "you",
        "Added in Settings",
        None,
    )
    .unwrap();
    let gone = forget(&c, Forget::Conversation("c1")).unwrap();
    assert_eq!(gone, std::slice::from_ref(&only.id));
    assert!(get(&c, &both.id).unwrap().is_some());
    assert!(
        get(&c, &mine.id).unwrap().is_some(),
        "no source, nothing to purge"
    );
    assert_eq!(
        forget(&c, Forget::Source(SourceKind::Message, "m9")).unwrap(),
        [both.id]
    );
}

#[test]
fn used_memories_are_recorded_per_message_and_deleted_ones_drop_out() {
    let c = db();
    let a = add_user(
        &c,
        Scope::Account,
        Kind::Fact,
        "you",
        "Lives in Lisbon",
        None,
    )
    .unwrap();
    let b = add_user(&c, Scope::Account, Kind::Fact, "you", "Has a cat", None).unwrap();
    record_used(
        &c,
        "msg1",
        "Weekly plan",
        &[(a.id.clone(), "retrieved"), (b.id.clone(), "profile")],
    )
    .unwrap();
    assert!(record_used(&c, "msg1", "x", &[(a.id.clone(), "guessed")]).is_err());
    let used = used_by(&c, "msg1").unwrap();
    assert_eq!(used.len(), 2);
    assert_eq!(used[0].0.last_used_in.as_deref(), Some("Weekly plan"));
    delete(&c, &b.id).unwrap();
    assert_eq!(used_by(&c, "msg1").unwrap().len(), 1);
}

#[test]
fn export_holds_every_item_with_sources_and_versions() {
    let c = db();
    add_user(
        &c,
        Scope::Account,
        Kind::Fact,
        "you",
        "Lives in Lisbon",
        Some(msg("m1", "c1")),
    )
    .unwrap();
    let e = export(&c).unwrap();
    assert_eq!(e["format"], "demios.memories.v1");
    assert_eq!(e["items"][0]["sources"][0]["title"], "Trip to Lisbon");
}

#[test]
fn undo_cant_bring_back_what_a_purge_removed_and_edits_survive_the_purge() {
    let c = db();
    let a = suggest(
        &c,
        suggestion(Scope::Account, "From chat 1", false, Kind::Fact),
    )
    .unwrap();
    let held = delete(&c, &a.id).unwrap();
    forget(&c, Forget::Conversation("c1")).unwrap();
    assert!(matches!(restore(&c, &held), Err(MemoryError::SourcesGone)));
    assert!(get(&c, &a.id).unwrap().is_none());
    // An item the person edited is theirs: deleting its chat keeps it.
    let b = suggest(
        &c,
        suggestion(Scope::Account, "Likes tea", false, Kind::Preference),
    )
    .unwrap();
    edit(
        &c,
        &b.id,
        Edit {
            text: "Likes green tea".into(),
            kind: Kind::Preference,
            subject: "you".into(),
            scope: Scope::Account,
        },
    )
    .unwrap();
    forget(&c, Forget::Conversation("c1")).unwrap();
    assert!(get(&c, &b.id).unwrap().is_some());
}

#[test]
fn an_item_cant_supersede_itself_or_move_between_add_ons_and_failures_leave_nothing_half_done() {
    let c = db();
    let a = add_user(&c, Scope::Account, Kind::Fact, "x", "y", None).unwrap();
    assert!(supersede(&c, &a.id, &a.id).is_err());
    let m = add_user(&c, Scope::Addon("mail".into()), Kind::Fact, "x", "y", None).unwrap();
    assert!(
        edit(
            &c,
            &m.id,
            Edit {
                text: "y".into(),
                kind: Kind::Fact,
                subject: "x".into(),
                scope: Scope::Addon("calendar".into())
            }
        )
        .is_err()
    );
    // A failed edit (empty text) leaves no version behind.
    assert!(
        edit(
            &c,
            &a.id,
            Edit {
                text: "  ".into(),
                kind: Kind::Fact,
                subject: "x".into(),
                scope: Scope::Account
            }
        )
        .is_err()
    );
    assert!(detail(&c, &a.id).unwrap().unwrap().versions.is_empty());
}
