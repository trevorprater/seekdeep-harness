//! Bundled badge registration, disposal, content, and artifact-integrity parity.

use sha2::{Digest as _, Sha256};

#[tokio::test]
async fn registers_loads_and_disposes_the_product_renamed_badge_skill() {
    let context = seekdeep_cordis::Context::new();
    let skills =
        seekdeep_skill::SkillRegistry::install(&context, &seekdeep_skill::Config::default())
            .unwrap();
    let fiber = context
        .plugin(seekdeep_skill_badge::plugin(), serde_json::Value::Null)
        .unwrap();
    fiber.await_settled().await.unwrap();
    let listed = skills
        .list(&seekdeep_skill::SkillViewOptions::default())
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    let summary = &listed[0];
    assert_eq!(summary.name, "seekdeep-badge");
    assert_eq!(summary.description, seekdeep_skill_badge::DESCRIPTION);
    assert!(summary.invocation.model_invocable);
    assert!(summary.invocation.user_invocable);
    assert_eq!(summary.provider, "seekdeep-badge");
    assert_eq!(summary.source.0, "bundled");
    assert_eq!(
        summary.resource_base,
        Some(seekdeep_skill::SkillResourceBase::Directory {
            path: seekdeep_skill_badge::resource_directory(),
        })
    );
    let loaded = skills
        .get(
            "seekdeep-badge",
            &seekdeep_skill::SkillViewOptions::default(),
        )
        .await
        .unwrap()
        .unwrap();
    assert!(
        loaded
            .content
            .contains("Preserve the badge's 121×20 dimensions")
    );
    assert_eq!(loaded.summary.resource_base, summary.resource_base);
    fiber.dispose().await.unwrap();
    assert!(
        skills
            .list(&seekdeep_skill::SkillViewOptions::default())
            .await
            .unwrap()
            .is_empty()
    );
    context.fiber().restart().await.unwrap();
}

#[test]
fn ships_the_official_726_by_120_png_unchanged() {
    let png = seekdeep_skill_badge::BADGE_PNG;
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 726);
    assert_eq!(u32::from_be_bytes(png[20..24].try_into().unwrap()), 120);
    assert_eq!(
        format!("{:x}", Sha256::digest(png)),
        "f2c4f5ec9cbe847c0c763545c4d839efa8485bc74203733d0a0e8259f233c653"
    );
}
