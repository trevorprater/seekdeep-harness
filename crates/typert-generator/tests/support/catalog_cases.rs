//! Shared authored model cases for native assertions and live source comparison.

use seekdeep_typert_generator::{
    Result,
    catalog::{
        CordisCatalogPolicy, CordisCatalogProjector, render_inherited_page, render_page_region,
    },
    model::{FaceModel, SourceDeclarationModel},
};
use serde_json::{Value, json};

pub(crate) fn cases() -> Vec<Value> {
    let mut cases = vec![base()];
    event_cases(&mut cases);
    service_cases(&mut cases);
    runtime_cases(&mut cases);
    invalid_pattern_cases(&mut cases);
    word_boundary_cases(&mut cases);
    cases
}

fn add(cases: &mut Vec<Value>, name: &str, edit: impl FnOnce(&mut Value)) {
    let mut case = base();
    case["name"] = json!(name);
    edit(&mut case);
    cases.push(case);
}

fn base() -> Value {
    let location = json!({"file":"packages/group/catalog/src/index.ts","line":10,"column":3});
    let export = json!({"subpath":".","name":"CatalogService","symbol":"service","aliases":["CatalogService"]});
    let parameter = json!({"name":"value","binding":"identifier","type":"known","optional":false,"rest":false,"receiver":false});
    let member = json!({
        "id":"run","kind":"method","name":"run","optional":false,"readonly":false,"async":false,"abstract":false,"static":false,"visibility":"public",
        "location":location,"text":"run(value: Known): Known","tags":[],
        "jsDoc":"/**\n * Run a value.\n * @param value - first line\n * continued value.\n * @returns - returned value.\n * @throws first failure\n * continued failure.\n * @throw second failure.\n */",
        "signature":{"typeParameters":[],"parameters":[parameter],"returns":"known"}
    });
    json!({
        "name":"complete-contract","page":"shell.md",
        "policy":{"linkedTypePages":{"Known":"core.md","Nested":"nested.md"},"foundationTypeNames":["Promise"],"typeLinkExemptions":{},"inheritedEvents":[{"name":"internal/test","summary":"Inherited event.","source":"vendor/test.ts:1"}],"inheritedServices":[{"name":"ctx.on","summary":"Listen.","source":"vendor/test.ts:2"}]},
        "sourceDeclarations":[
            {"face":"host","package":"@fixture/catalog","name":"Known","kind":"interface","location":{"file":"packages/group/catalog/src/types.ts","line":1,"column":1},"text":"export interface Known { nested: Nested }"},
            {"face":"host","package":"@fixture/catalog","name":"Nested","kind":"alias","location":{"file":"packages/group/catalog/src/types.ts","line":2,"column":1},"text":"export type Nested = string"}
        ],
        "face":{"face":"host","packages":[{
            "name":"@fixture/catalog","root":"packages/group/catalog","exports":[export],
            "services":[{"key":"demo","symbol":"service","export":export,"members":["run"],"location":location,"tags":[]}],
            "events":[{"name":"demo/changed","signature":"event","text":"'demo/changed'(value: Known): void","mode":"emit","location":location,"tags":[],"jsDoc":"/**\n * A value changed. More detail.\n * @param value - the value.\n * @mode emit\n */"}],
            "objects":[],"schemas":[],"invocations":[]
        }],"graph":{
            "declarations":[{"id":"service","package":"@fixture/catalog","name":"CatalogService","kind":"class","abstract":false,"exported":true,"location":location,"text":"export class CatalogService","typeParameters":[],"extends":[],"implements":[],"members":[member],"tags":[],"jsDoc":"/**\n * First sentence. More detail.\n *\n * - First item\n *   continued.\n * - Second {@link Known} item.\n */"}],
            "nodes":[
                {"id":"void","kind":"keyword","name":"void"},
                {"id":"known","kind":"reference","name":"Known","arguments":[],"target":{"kind":"standard","name":"Known"}},
                {"id":"event","kind":"function","signature":{"typeParameters":[],"parameters":[parameter],"returns":"void"}}
            ]
        }}
    })
}

fn event_cases(cases: &mut Vec<Value>) {
    add(cases, "missing-mode", |case| {
        case["face"]["packages"][0]["events"][0]
            .as_object_mut()
            .unwrap()
            .remove("mode");
    });
    add(cases, "invalid-mode", |case| {
        case["face"]["packages"][0]["events"][0]["mode"] = json!("unknown");
    });
    add(cases, "event-not-callable", |case| {
        case["face"]["packages"][0]["events"][0]["signature"] = json!("void");
    });
    add(cases, "waterfall-without-next", |case| {
        case["face"]["packages"][0]["events"][0]["mode"] = json!("waterfall");
    });
    for (name, mode) in [
        ("waterfall-next", "waterfall"),
        ("non-waterfall-next", "emit"),
    ] {
        add(cases, name, |case| {
            case["face"]["packages"][0]["events"][0]["mode"] = json!(mode);
            let parameters = case["face"]["graph"]["nodes"][2]["signature"]["parameters"]
                .as_array_mut()
                .unwrap();
            let mut next = parameters[0].clone();
            next["name"] = json!("next");
            next["type"] = json!("void");
            parameters.push(next);
        });
    }
    for (name, doc) in [
        ("event-missing-param", "/** Event.\n * @mode emit\n */"),
        (
            "event-empty-param",
            "/** Event.\n * @param value\n * @mode emit\n */",
        ),
        (
            "event-stale-param",
            "/** Event.\n * @param value yes\n * @param absent no\n * @mode emit\n */",
        ),
        (
            "event-empty-description",
            "/** @param value yes\n * @mode emit\n */",
        ),
        ("event-deprecated", "/** @deprecated Superseded. */"),
    ] {
        add(cases, name, |case| {
            case["face"]["packages"][0]["events"][0]["jsDoc"] = json!(doc);
        });
    }
    add(cases, "event-binding-pattern", |case| {
        case["face"]["graph"]["nodes"][2]["signature"]["parameters"][0]["binding"] =
            json!("object");
    });
    add(cases, "event-receiver", |case| {
        case["face"]["graph"]["nodes"][2]["signature"]["parameters"][0]["receiver"] = json!(true);
        case["face"]["packages"][0]["events"][0]["jsDoc"] = json!("/** Event.\n * @mode emit\n */");
    });
    add(cases, "unclassified-type", |case| {
        case["policy"]["linkedTypePages"] = json!({});
    });
    add(cases, "documentation-before-classification", |case| {
        case["policy"]["linkedTypePages"] = json!({});
        case["face"]["packages"][0]["events"][0]["jsDoc"] = json!("/** @mode emit */");
    });
    add(cases, "event-aggregates", |case| {
        let event = &mut case["face"]["packages"][0]["events"][0];
        event.as_object_mut().unwrap().remove("mode");
        let mut second = event.clone();
        second["name"] = json!("demo/second");
        case["face"]["packages"][0]["events"]
            .as_array_mut()
            .unwrap()
            .push(second);
    });
}

fn service_cases(cases: &mut Vec<Value>) {
    for (name, doc) in [
        (
            "method-empty-description",
            Some("/** @param value yes\n * @returns result\n */"),
        ),
        (
            "method-missing-param",
            Some("/** Run.\n * @returns result\n */"),
        ),
        (
            "method-empty-param",
            Some("/** Run.\n * @param value\n * @returns result\n */"),
        ),
        (
            "method-missing-returns",
            Some("/** Run.\n * @param value yes\n */"),
        ),
        (
            "method-empty-returns",
            Some("/** Run.\n * @param value yes\n * @returns\n */"),
        ),
        (
            "method-stale-param",
            Some("/** Run.\n * @param value yes\n * @param other no\n * @returns result\n */"),
        ),
        ("method-no-jsdoc", None),
        ("method-deprecated", Some("/** @deprecated Superseded. */")),
    ] {
        add(cases, name, |case| {
            let member = &mut case["face"]["graph"]["declarations"][0]["members"][0];
            if let Some(doc) = doc {
                member["jsDoc"] = json!(doc);
            } else {
                member.as_object_mut().unwrap().remove("jsDoc");
            }
        });
    }
    add(cases, "service-no-jsdoc", |case| {
        case["face"]["graph"]["declarations"][0]
            .as_object_mut()
            .unwrap()
            .remove("jsDoc");
    });
    add(cases, "service-deprecated", |case| {
        case["face"]["graph"]["declarations"][0]["jsDoc"] = json!("/** @deprecated Superseded. */");
    });
    add(cases, "method-void-no-returns", |case| {
        let member = &mut case["face"]["graph"]["declarations"][0]["members"][0];
        member["signature"]["returns"] = json!("void");
        member["jsDoc"] = json!("/** Run.\n * @param value yes\n */");
    });
    add(cases, "method-binding-pattern", |case| {
        case["face"]["graph"]["declarations"][0]["members"][0]["signature"]["parameters"][0]["binding"] =
            json!("array");
    });
    add(cases, "nested-host-merge-excluded", |case| {
        case["face"]["packages"][0]["services"][0]["location"]["file"] =
            json!("packages/group/catalog/src/nested/index.ts");
    });
    add(cases, "foreign-service-declaration-excluded", |case| {
        case["face"]["graph"]["declarations"][0]["location"]["file"] =
            json!("packages/group/other/src/index.ts");
    });
    add(cases, "interface-declaration", |case| {
        case["face"]["graph"]["declarations"][0]["kind"] = json!("interface");
    });
    add(cases, "alias-service-excluded", |case| {
        case["face"]["graph"]["declarations"][0]["kind"] = json!("alias");
    });
    add(cases, "abstract-service", |case| {
        case["face"]["graph"]["declarations"][0]["abstract"] = json!(true);
    });
    add(cases, "documented-property", |case| {
        let member = &mut case["face"]["graph"]["declarations"][0]["members"][0];
        member["kind"] = json!("property");
        member["type"] = json!("known");
        member["text"] = json!("value: Known");
        member.as_object_mut().unwrap().remove("signature");
    });
    add(cases, "computed-member-excluded", |case| {
        case["face"]["graph"]["declarations"][0]["members"][0]["name"] = json!("[symbol]");
    });
    service_precedence_cases(cases);
}

fn service_precedence_cases(cases: &mut Vec<Value>) {
    for (name, winning_class) in [
        ("class-precedes-interface", true),
        ("later-class-replaces-interface", false),
    ] {
        add(cases, name, |case| {
            let declarations = case["face"]["graph"]["declarations"]
                .as_array_mut()
                .unwrap();
            let mut other = declarations[0].clone();
            other["id"] = json!("other");
            other["name"] = json!("OtherService");
            other["members"] = json!([]);
            if winning_class {
                other["kind"] = json!("interface");
                other.as_object_mut().unwrap().remove("jsDoc");
            } else {
                declarations[0]["kind"] = json!("interface");
                declarations[0].as_object_mut().unwrap().remove("jsDoc");
            }
            declarations.push(other);
            let services = case["face"]["packages"][0]["services"]
                .as_array_mut()
                .unwrap();
            let mut other = services[0].clone();
            other["symbol"] = json!("other");
            other["members"] = json!([]);
            services.push(other);
        });
    }
}

fn runtime_cases(cases: &mut Vec<Value>) {
    add(cases, "same-page-links-omitted", |case| {
        case["page"] = json!("core.md");
    });
    add(cases, "runtime-service-exclusion", |case| {
        case["policy"]["runtimeServiceExclusions"] = json!(["demo"]);
    });
    add(cases, "curated-runtime-service", |case| {
        case["policy"]["runtimeServices"] = json!([{"key":"_timer","type":"Timer","abstract":false,"doc":"Timer. More detail.","methods":[],"source":"vendor/timer.ts:1"}]);
    });
    add(cases, "ambiguous-runtime-type", |case| {
        let duplicate = case["sourceDeclarations"][0].clone();
        case["sourceDeclarations"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
    });
    add(cases, "foreign-face-runtime-type", |case| {
        case["sourceDeclarations"][0]["face"] = json!("client");
    });
    add(cases, "enum-runtime-type-excluded", |case| {
        case["sourceDeclarations"][0]["kind"] = json!("enum");
    });
    add(cases, "runtime-type-truncation", |case| {
        case["sourceDeclarations"][0]["text"] =
            json!(format!("type Known = '{}'; Nested", "x".repeat(1600)));
    });
    add(cases, "unicode-anchors", |case| {
        case["face"]["packages"][0]["services"][0]["key"] = json!("ΔÉé-名 ②");
    });
    add(cases, "client-face-classification-bypass", |case| {
        case["face"]["face"] = json!("client");
        case["policy"]["linkedTypePages"] = json!({});
        case["face"]["packages"][0]["services"][0]["location"]["file"] =
            json!("packages/group/catalog/src/client/index.tsx");
        for declaration in case["sourceDeclarations"].as_array_mut().unwrap() {
            declaration["face"] = json!("client");
        }
    });
    add(cases, "duplicate-tag-and-optional-name", |case| {
        case["face"]["graph"]["declarations"][0]["members"][0]["jsDoc"] = json!(
            "/** Run.\n * @param [value] — first\n * @param value – second\n * continuation.\n *\n * ignored after blank.\n * @return returned\n * @throws failure\n * @unknown ends the sink\n * ignored\n */"
        );
    });
    add(cases, "js-whitespace", |case| {
        case["face"]["graph"]["declarations"][0]["members"][0]["jsDoc"] = json!(
            "/**\n * \u{feff}Run.\n * @param\u{a0}value\u{a0}-\u{a0}accepted.\n * @returns\u{3000}returned.\n */"
        );
    });
    add(cases, "cyclic-signature-graph", |case| {
        case["face"]["graph"]["nodes"][1] = json!({"id":"known","kind":"array","element":"known"});
        case["face"]["graph"]["declarations"][0]["members"][0]["signature"]["returns"] =
            json!("void");
    });
}

pub(crate) fn outcome(case: &Value) -> Value {
    match project(case) {
        Ok(value) => json!({"ok":value}),
        Err(error) => json!({"error":{"name":error.name(),"message":error.to_string()}}),
    }
}

fn invalid_pattern_cases(cases: &mut Vec<Value>) {
    for name in ["[", "[z-a]", "(", ")", "*", "(?"] {
        add(cases, &format!("invalid-linked-pattern-{name}"), |case| {
            case["policy"]["linkedTypePages"][name] = json!("invalid.md");
        });
    }
    add(cases, "invalid-declaration-pattern", |case| {
        case["sourceDeclarations"][0]["name"] = json!("[");
    });
}

fn word_boundary_cases(cases: &mut Vec<Value>) {
    for text in [
        "KnownSuffix",
        "_Known",
        "Known_1",
        "éKnown中",
        "Known Known",
    ] {
        add(cases, &format!("word-boundaries-{text}"), |case| {
            case["face"]["graph"]["declarations"][0]["members"][0]["text"] =
                json!(format!("run(value: {text}): void"));
            case["face"]["packages"][0]["events"][0]["text"] =
                json!(format!("'demo/changed'(value: {text}): void"));
        });
    }
    add(cases, "type-link-regex-metacharacter", |case| {
        case["policy"]["linkedTypePages"]["K.own"] = json!("wildcard.md");
    });
}

fn project(case: &Value) -> Result<Value> {
    let face: FaceModel = serde_json::from_value(case["face"].clone()).expect("case face");
    let declarations: Vec<SourceDeclarationModel> =
        serde_json::from_value(case["sourceDeclarations"].clone())
            .expect("case source declarations");
    let policy: CordisCatalogPolicy =
        serde_json::from_value(case["policy"].clone()).expect("case policy");
    let projector = CordisCatalogProjector::new(&face, &declarations, &policy);
    let model = projector.project()?;
    let runtime = projector.render_runtime_api(&model)?;
    let region = render_page_region(
        case["page"].as_str().expect("case page"),
        &model.services,
        &model.events,
        &policy,
    )?;
    Ok(
        json!({"model":model,"runtime":runtime,"catalog":projector.runtime_catalog(&model)?,"region":region,"inherited":render_inherited_page(&policy)}),
    )
}
