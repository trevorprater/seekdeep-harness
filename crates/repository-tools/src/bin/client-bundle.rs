//! JSON binding for the Rust Client bundle preset and CSS compiler.

use std::{
    io::{self, BufRead as _},
    path::PathBuf,
};

use seekdeep_repository_tools::client_bundle::{self, ClientBundleOptions, ClientModuleId};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "camelCase")]
enum Request {
    Preset {
        id: ClientModuleId,
        entries: Vec<String>,
        #[serde(default)]
        options: ClientBundleOptions,
        #[serde(default, deserialize_with = "present_face")]
        face: Option<Value>,
        #[serde(rename = "nodeEnv")]
        node_env: Option<String>,
    },
    Library {
        id: ClientModuleId,
        entries: Vec<String>,
        #[serde(default, deserialize_with = "present_face")]
        face: Option<Value>,
    },
    Only {
        configs: Vec<Value>,
        #[serde(default, deserialize_with = "present_face")]
        face: Option<Value>,
    },
    Import {
        specifier: String,
    },
    SourceMap {
        source: String,
        sourcemap: PathBuf,
        repository: PathBuf,
    },
    Css {
        id: ClientModuleId,
        file: PathBuf,
    },
    Asset {
        source: String,
        importer: PathBuf,
    },
}

fn present_face<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}

fn dispatch(request: Request) -> anyhow::Result<Value> {
    Ok(match request {
        Request::Preset {
            id,
            entries,
            options,
            face,
            node_env,
        } => serde_json::to_value(client_bundle::client_bundle(
            &id,
            &entries,
            &options,
            client_bundle::build_face(face.as_ref())?,
            node_env.as_deref(),
        ))?,
        Request::Library { id, entries, face } => {
            serde_json::to_value(client_bundle::client_only(
                &[client_bundle::client_library_config(
                    &id,
                    &entries,
                    &serde_json::Map::new(),
                )],
                client_bundle::build_face(face.as_ref())?,
            ))?
        }
        Request::Only { configs, face } => serde_json::to_value(client_bundle::client_only(
            &configs,
            client_bundle::build_face(face.as_ref())?,
        ))?,
        Request::Import { specifier } => {
            client_bundle::check_import(&specifier)?;
            json!({ "external": client_bundle::is_external(&specifier) })
        }
        Request::SourceMap {
            source,
            sourcemap,
            repository,
        } => json!(client_bundle::browser_source_path(
            &source,
            &sourcemap,
            &repository
        )),
        Request::Css { id, file } => {
            serde_json::to_value(client_bundle::compile_css_module(&id, &file)?)?
        }
        Request::Asset { source, importer } => {
            serde_json::to_value(client_bundle::source_asset_path(&source, &importer))?
        }
    })
}

fn main() -> anyhow::Result<()> {
    for line in io::stdin().lock().lines() {
        let result = serde_json::from_str::<Request>(&line?)
            .map_err(anyhow::Error::from)
            .and_then(dispatch);
        let response = match result {
            Ok(value) => json!({ "value": value }),
            Err(error) => json!({ "error": error.to_string() }),
        };
        println!("{}", serde_json::to_string(&response)?);
    }
    Ok(())
}
