//! Pure judgement of the Cordis catalog commands: the Client subset filter,
//! the identity renames, and artifact freshness comparison.

use seekdeep_typert_generator::catalog::{
    CordisCatalogModel, EventEntry, Mode, ServiceEntry, ServiceMethodEntry,
};
use xtask::cordis_catalog::{
    CLIENT_EVENTS, CLIENT_SERVICES, EVENT_SCOPE_PAGE, SERVICE_PAGE, client_model,
    cordis_catalog_policy, document_identity, partition_maps, target_identity,
};

fn method(signature: &str) -> ServiceMethodEntry {
    ServiceMethodEntry {
        kind: None,
        signature: signature.to_owned(),
        js_doc: String::new(),
    }
}

fn service(key: &str, methods: &[&str]) -> ServiceEntry {
    ServiceEntry {
        key: key.to_owned(),
        type_name: "Svc".to_owned(),
        is_abstract: false,
        doc: String::new(),
        methods: methods.iter().map(|signature| method(signature)).collect(),
        source: "packages/x/y/src/index.ts:1".to_owned(),
    }
}

fn event(name: &str) -> EventEntry {
    EventEntry {
        name: name.to_owned(),
        scope: name.split('/').next().unwrap_or_default().to_owned(),
        signature: String::new(),
        js_doc: String::new(),
        mode: Mode::Emit,
        doc: String::new(),
        source: String::new(),
    }
}

#[test]
fn client_model_keeps_curated_services_reachable_methods_and_client_events() {
    let model = CordisCatalogModel {
        services: vec![
            service(
                "layout",
                &[
                    "toggleSidebar(): void",
                    "declare readonly openDetails: (id: string) => void",
                    "async closeDetails(): Promise<void>",
                    "hidden(): void",
                ],
            ),
            service("llm", &["generate(): void"]),
        ],
        events: vec![event("theme/change"), event("llm/request")],
    };
    let client = client_model(&model);
    assert_eq!(client.services.len(), 1);
    assert_eq!(
        client.services[0]
            .methods
            .iter()
            .map(|method| method.signature.as_str())
            .collect::<Vec<_>>(),
        [
            "toggleSidebar(): void",
            "declare readonly openDetails: (id: string) => void",
            "async closeDetails(): Promise<void>",
        ]
    );
    assert_eq!(
        client
            .events
            .iter()
            .map(|event| event.name.as_str())
            .collect::<Vec<_>>(),
        ["theme/change"]
    );
    assert!(CLIENT_SERVICES.iter().any(|(key, _)| *key == "workspaces"));
    assert!(CLIENT_EVENTS.contains(&"slots/changed"));
}

#[test]
fn identity_renames_data_and_documentation_spellings() {
    assert_eq!(
        target_identity("@deepseek-ai/dsh-scope dsh-agent DSH_HOME DshEnvironment DSH process"),
        "@seekdeep-ai/seekdeep-scope seekdeep-agent SEEKDEEP_HOME SeekdeepEnvironment SeekDeep Harness process"
    );
    assert_eq!(
        target_identity("dsh.client @dshScopeScan"),
        "dsh.client @dshScopeScan"
    );
    assert_eq!(
        document_identity("dsh.client @dshScopeScan dsh-x"),
        "seekdeep.client @seekdeepScopeScan seekdeep-x"
    );
}

#[test]
fn policy_tables_are_consistent_with_each_other() {
    let policy = cordis_catalog_policy();
    let maps = partition_maps();
    assert_eq!(maps.service_page.len(), SERVICE_PAGE.len());
    assert_eq!(maps.event_scope_page.len(), EVENT_SCOPE_PAGE.len());
    for page in maps
        .service_page
        .values()
        .chain(maps.event_scope_page.values())
    {
        assert!(
            std::path::Path::new(page)
                .extension()
                .is_some_and(|extension| extension == "md"),
            "{page}"
        );
    }
    assert!(policy.foundation_type_names.contains("Promise"));
    assert_eq!(
        policy
            .runtime_services
            .as_ref()
            .map(|services| services[0].key.as_str()),
        Some("timer")
    );
    assert_eq!(policy.inherited_events.len(), 15);
    assert_eq!(policy.inherited_services.len(), 10);
}
