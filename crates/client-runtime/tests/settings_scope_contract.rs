//! Client Settings scope type-contract parity.

use std::rc::Rc;

use futures::FutureExt;
use seekdeep_client_runtime::{
    ClientSettingsMode, ClientSettingsScope, ClientSettingsScopeSnapshot, ClientSettingsStatus,
    RuntimeDisposer,
};
use serde_json::{Value, json};

struct Scope {
    snapshot: Rc<ClientSettingsScopeSnapshot<Value>>,
}

impl ClientSettingsScope<Value> for Scope {
    fn snapshot(&self) -> Rc<ClientSettingsScopeSnapshot<Value>> {
        self.snapshot.clone()
    }

    fn subscribe(&self, _listener: Rc<dyn Fn()>) -> RuntimeDisposer {
        RuntimeDisposer::new(|| {})
    }

    fn set(&self, _field: String, _value: Value) -> futures::future::LocalBoxFuture<'static, ()> {
        futures::future::ready(()).boxed_local()
    }

    fn unset(&self, _field: String) -> futures::future::LocalBoxFuture<'static, ()> {
        futures::future::ready(()).boxed_local()
    }
}

#[test]
fn scope_contract_retains_presence_layers_revision_writability_and_mode() {
    let scope = Scope {
        snapshot: Rc::new(ClientSettingsScopeSnapshot {
            status: ClientSettingsStatus::Ready,
            value: Some(Rc::new(json!({"model":"reasoner"}))),
            base: json!({"model":"chat"}),
            user: json!({"model":"reasoner"}),
            revision: Some(7),
            writable: true,
            mode: ClientSettingsMode::Host,
        }),
    };
    let snapshot = scope.snapshot();
    assert_eq!(snapshot.status, ClientSettingsStatus::Ready);
    assert_eq!(
        snapshot.value.as_deref(),
        Some(&json!({"model":"reasoner"}))
    );
    assert_eq!(snapshot.base, json!({"model":"chat"}));
    assert!(snapshot.user.get("model").is_some());
    assert_eq!(snapshot.revision, Some(7));
    assert!(snapshot.writable);
    assert_eq!(snapshot.mode, ClientSettingsMode::Host);
}
