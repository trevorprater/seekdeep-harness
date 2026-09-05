//! Browser-persistent controller face and React/WASM boundary helpers.

use std::{cell::RefCell, rc::Rc};

use js_sys::{Object, Reflect};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use wasm_bindgen::{JsValue, closure::Closure, prelude::wasm_bindgen};

use crate::{
    CreateLaunch, DirectoryBrowserController, DirectoryEntry, DirectoryListing, LandingOptions,
    ListingLaunch, PreviewToken, TargetLanding,
};

fn object(entries: &[(&str, JsValue)]) -> Result<Object, JsValue> {
    let object = Object::new();
    for (key, value) in entries {
        Reflect::set(&object, &JsValue::from_str(key), value)?;
    }
    Ok(object)
}

fn encode<T: Serialize>(value: &T) -> Result<JsValue, JsValue> {
    value
        .serialize(&serde_wasm_bindgen::Serializer::new().serialize_maps_as_objects(true))
        .map_err(|error| js_sys::Error::new(&error.to_string()).into())
}

fn decode<T: DeserializeOwned>(value: JsValue, owner: &str) -> Result<T, JsValue> {
    serde_wasm_bindgen::from_value(value)
        .map_err(|error| js_sys::TypeError::new(&format!("invalid {owner}: {error}")).into())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BeginLandingPayload {
    #[serde(default)]
    path: Option<String>,
    options: LandingOptions,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TargetLandedPayload {
    launch: ListingLaunch,
    target: DirectoryListing,
    options: LandingOptions,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ParentLandedPayload {
    seq: u64,
    parent: DirectoryListing,
}

#[derive(Deserialize)]
struct SeqPayload {
    seq: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FailurePayload {
    seq: u64,
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TargetFailurePayload {
    seq: u64,
    options: LandingOptions,
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListingPayload {
    seq: u64,
    listing: DirectoryListing,
}

#[derive(Deserialize)]
struct DraftPayload {
    draft: String,
}

#[derive(Deserialize)]
struct FocusPayload {
    focus: bool,
}

#[derive(Deserialize)]
struct CreateSuccessPayload {
    launch: CreateLaunch,
    #[serde(rename = "createdPath")]
    created_path: String,
}

#[derive(Deserialize)]
struct CreateFailurePayload {
    launch: CreateLaunch,
    message: String,
}

fn target_outcome(outcome: TargetLanding) -> Result<JsValue, JsValue> {
    match outcome {
        TargetLanding::Stale => Ok(object(&[("kind", JsValue::from_str("stale"))])?.into()),
        TargetLanding::CommittedSingle => {
            Ok(object(&[("kind", JsValue::from_str("committedSingle"))])?.into())
        }
        TargetLanding::Parent(parent) => Ok(object(&[
            ("kind", JsValue::from_str("parent")),
            ("parent", encode(&parent)?),
        ])?
        .into()),
    }
}

#[allow(clippy::too_many_lines)] // Closed action dispatch keeps every mutation on one audited face.
fn controller_face() -> Result<JsValue, JsValue> {
    let controller = Rc::new(RefCell::new(DirectoryBrowserController::new()));
    let snapshot_controller = controller.clone();
    let snapshot = Closure::wrap(
        Box::new(move || encode(snapshot_controller.borrow().state()))
            as Box<dyn FnMut() -> Result<JsValue, JsValue>>,
    );
    let dispatch_controller = controller;
    let dispatch = Closure::wrap(Box::new(
        move |action: String, payload: JsValue| -> Result<JsValue, JsValue> {
            let mut controller = dispatch_controller.borrow_mut();
            match action.as_str() {
                "open" => encode(&controller.open()),
                "close" => {
                    controller.close();
                    Ok(JsValue::UNDEFINED)
                }
                "supersede" => encode(&controller.supersede()),
                "dispose" => {
                    controller.dispose();
                    Ok(JsValue::UNDEFINED)
                }
                "beginLanding" => {
                    let payload: BeginLandingPayload = decode(payload, "beginLanding payload")?;
                    encode(&controller.begin_landing(payload.path, payload.options))
                }
                "targetLanded" => {
                    let payload: TargetLandedPayload = decode(payload, "targetLanded payload")?;
                    target_outcome(controller.target_landed(
                        &payload.launch,
                        payload.target,
                        payload.options,
                    ))
                }
                "parentLanded" => {
                    let payload: ParentLandedPayload = decode(payload, "parentLanded payload")?;
                    encode(&controller.parent_landed(payload.seq, payload.parent))
                }
                "parentFailed" => {
                    let payload: SeqPayload = decode(payload, "parentFailed payload")?;
                    encode(&controller.parent_failed(payload.seq))
                }
                "parentWaitElapsed" => {
                    let payload: SeqPayload = decode(payload, "parentWait payload")?;
                    encode(&controller.parent_wait_elapsed(payload.seq))
                }
                "targetFailed" => {
                    let payload: TargetFailurePayload = decode(payload, "targetFailed payload")?;
                    encode(&controller.target_failed(payload.seq, payload.options, payload.message))
                }
                "beginSelection" => {
                    let entry: DirectoryEntry = decode(payload, "selection entry")?;
                    encode(&controller.begin_selection(entry))
                }
                "selectionLanded" => {
                    let payload: ListingPayload = decode(payload, "selectionLanded payload")?;
                    encode(&controller.selection_landed(payload.seq, payload.listing))
                }
                "selectionFailed" => {
                    let payload: FailurePayload = decode(payload, "selectionFailed payload")?;
                    encode(&controller.selection_failed(payload.seq, payload.message))
                }
                "advance" => {
                    let entry: DirectoryEntry = decode(payload, "advance entry")?;
                    encode(&controller.advance(entry))
                }
                "openPathEditor" => {
                    controller.open_path_editor();
                    Ok(JsValue::UNDEFINED)
                }
                "editPath" => {
                    let payload: DraftPayload = decode(payload, "editPath payload")?;
                    encode(&controller.edit_path(payload.draft))
                }
                "previewElapsed" => {
                    let token: PreviewToken = decode(payload, "preview token")?;
                    encode(&controller.preview_elapsed(&token))
                }
                "submitPath" => encode(&controller.submit_path()),
                "cancelPathEdit" => {
                    let payload: FocusPayload = decode(payload, "cancelPathEdit payload")?;
                    encode(&controller.cancel_path_edit(payload.focus))
                }
                "toggleShowHidden" => {
                    controller.toggle_show_hidden();
                    Ok(JsValue::UNDEFINED)
                }
                "openCreateDialog" => encode(&controller.open_create_dialog()),
                "editFolderName" => {
                    let payload: DraftPayload = decode(payload, "editFolderName payload")?;
                    controller.edit_folder_name(payload.draft);
                    Ok(JsValue::UNDEFINED)
                }
                "closeCreateDialog" => encode(&controller.close_create_dialog()),
                "confirmCreate" => encode(&controller.confirm_create()),
                "creationSucceeded" => {
                    let payload: CreateSuccessPayload =
                        decode(payload, "creationSucceeded payload")?;
                    encode(&controller.creation_succeeded(&payload.launch, payload.created_path))
                }
                "creationFailed" => {
                    let payload: CreateFailurePayload = decode(payload, "creationFailed payload")?;
                    encode(&controller.creation_failed(&payload.launch, payload.message))
                }
                "creationRelistLanded" => {
                    let payload: ListingPayload = decode(payload, "creationRelistLanded payload")?;
                    encode(&controller.creation_relist_landed(payload.seq, payload.listing))
                }
                "creationRelistFailed" => {
                    let payload: FailurePayload = decode(payload, "creationRelistFailed payload")?;
                    encode(&controller.creation_relist_failed(payload.seq, payload.message))
                }
                "slowScanElapsed" => {
                    let payload: SeqPayload = decode(payload, "slowScan payload")?;
                    encode(&controller.slow_scan_elapsed(payload.seq))
                }
                "consumeFocus" => encode(&controller.consume_focus()),
                action => Err(js_sys::TypeError::new(&format!(
                    "unknown directory browser action {action:?}"
                ))
                .into()),
            }
        },
    )
        as Box<dyn FnMut(String, JsValue) -> Result<JsValue, JsValue>>);
    Ok(object(&[
        ("snapshot", snapshot.into_js_value()),
        ("dispatch", dispatch.into_js_value()),
    ])?
    .into())
}

/// Creates one persistent controller face for a React component instance.
///
/// # Errors
///
/// Returns JavaScript object construction failures.
#[wasm_bindgen(js_name = createDirectoryBrowserStateController)]
pub fn create_directory_browser_state_controller() -> Result<JsValue, JsValue> {
    controller_face()
}
