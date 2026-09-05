//! Session-keyed catalog, single-flight, invalidation, lexicon, and pick parity.

use seekdeep_client_ui_skill::{
    SkillCandidate, SkillCatalogCache, SkillCatalogDecision, SkillCatalogEntry,
};
use seekdeep_identity::SessionId;

fn catalog() -> Vec<SkillCatalogEntry> {
    vec![
        SkillCatalogEntry {
            name: "commit-helper".to_owned(),
            description: "commit flow".to_owned(),
            when_to_use: None,
            model_invocable: true,
        },
        SkillCatalogEntry {
            name: "code-review".to_owned(),
            description: "review flow".to_owned(),
            when_to_use: Some("reviews".to_owned()),
            model_invocable: true,
        },
        SkillCatalogEntry {
            name: "user-only-skill".to_owned(),
            description: "user surface only".to_owned(),
            when_to_use: None,
            model_invocable: false,
        },
    ]
}

#[test]
fn addressed_settled_and_single_flight_decisions_are_exact() {
    let session = SessionId::new("s1");
    let mut cache = SkillCatalogCache::default();
    assert_eq!(cache.begin(&session, true), SkillCatalogDecision::Addressed);
    let SkillCatalogDecision::Start(generation) = cache.begin(&session, false) else {
        panic!("cold Session must start a generation");
    };
    assert_eq!(
        cache.begin(&session, false),
        SkillCatalogDecision::Join(generation)
    );
    assert!(cache.settle_success(&session, generation, catalog()));
    assert_eq!(
        cache.begin(&session, false),
        SkillCatalogDecision::Settled(catalog())
    );
    assert_eq!(
        cache.lexicon(&session),
        Some(vec![
            "commit-helper".to_owned(),
            "code-review".to_owned(),
            "user-only-skill".to_owned(),
        ])
    );
}

#[test]
fn failures_invalidation_and_stale_settlement_preserve_exact_ownership() {
    let one = SessionId::new("s1");
    let two = SessionId::new("s2");
    let mut cache = SkillCatalogCache::default();
    let SkillCatalogDecision::Start(first) = cache.begin(&one, false) else {
        panic!("cold Session must start");
    };
    assert!(cache.settle_failure(&one, first));
    let SkillCatalogDecision::Start(retry) = cache.begin(&one, false) else {
        panic!("failed Session must retry");
    };
    assert_ne!(first, retry);
    assert_eq!(cache.invalidate(&one), Some(retry));
    assert!(!cache.settle_success(&one, retry, catalog()));

    let SkillCatalogDecision::Start(one_generation) = cache.begin(&one, false) else {
        panic!("invalidated Session must restart");
    };
    let SkillCatalogDecision::Start(two_generation) = cache.begin(&two, false) else {
        panic!("independent Session must start");
    };
    assert_eq!(
        cache.clear(),
        vec![(one.clone(), one_generation), (two.clone(), two_generation)]
    );
    assert_eq!(cache.lexicon(&one), None);
    assert_eq!(cache.lexicon(&two), None);
}

#[test]
fn prefix_user_only_and_pick_copy_match_the_source() {
    assert_eq!(
        SkillCatalogCache::candidates(&catalog(), "co", "仅用户"),
        vec![
            SkillCandidate {
                name: "commit-helper".to_owned(),
                description: "commit flow".to_owned(),
            },
            SkillCandidate {
                name: "code-review".to_owned(),
                description: "review flow".to_owned(),
            },
        ]
    );
    assert_eq!(
        SkillCatalogCache::candidates(&catalog(), "user", "仅用户"),
        vec![SkillCandidate {
            name: "user-only-skill".to_owned(),
            description: "仅用户 · user surface only".to_owned(),
        }]
    );
    assert!(SkillCatalogCache::candidates(&catalog(), "Co", "仅用户").is_empty());
    assert_eq!(
        SkillCatalogCache::picked_text("commit-helper"),
        "/commit-helper "
    );
}

#[test]
fn catalog_rows_ignore_future_fields_like_javascript_property_reads() {
    let row = serde_json::from_value::<SkillCatalogEntry>(serde_json::json!({
        "name": "future-skill",
        "description": "forward compatible",
        "modelInvocable": true,
        "futureCapability": { "enabled": true }
    }))
    .unwrap();
    assert_eq!(
        row,
        SkillCatalogEntry {
            name: "future-skill".to_owned(),
            description: "forward compatible".to_owned(),
            when_to_use: None,
            model_invocable: true,
        }
    );
}
