//! Effect plug-in commands: list the registered plug-in effects and load OpenFX plug-ins
//! (docs/ofx.md).
//!
//! OpenFX plug-ins are native code, so loading them is up to the host: apps built with the
//! `openfx` feature set [`Session::ofx_loader`] (and the search paths and blocklist reset) from
//! `filmcraft-ofx`. Without it the OFX commands are disabled ("OpenFX plug-ins are not available
//! in this build"). Loaded plug-ins register in `filmcraft_project::plugin` for the rest of the
//! process and behave like built-in Video Effects.

use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, str_p};
use crate::{EngineError, Result, Session};

const NO_OFX: &str = "OpenFX plug-ins are not available in this build";

/// The OpenFX commands need a host built with the `openfx` feature.
fn has_ofx(s: &Session) -> std::result::Result<(), String> {
    if s.ofx_loader.is_some() { Ok(()) } else { Err(NO_OFX.into()) }
}

/// `effects.plugins.list`: every registered plug-in effect.
fn list(s: &mut Session, _: &Value) -> Result<Value> {
    use filmcraft_project::plugin;
    let list: Vec<Value> = plugin::plugin_defs()
        .into_iter()
        .map(|d| {
            let source = plugin::plugin(d.id).map(|p| p.source()).unwrap_or_default();
            json!({
                "id": d.id,
                "name": d.name,
                "category": d.category,
                "source": source,
                "params": d.params.iter().map(|p| p.id).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(json!({"api": plugin::PLUGIN_API_VERSION, "ofx": s.ofx_loader.is_some(), "plugins": list}))
}

/// The number of effects a loader result lists.
fn loaded_count(r: &Value) -> usize {
    r.get("loaded").and_then(Value::as_array).map_or(0, Vec::len)
}

/// `effects.plugins.loadOfx`: load OpenFX plug-ins (an `.ofx` binary, an `.ofx.bundle` folder or
/// a folder scanned recursively) through [`Session::ofx_loader`].
fn load_ofx(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "effects.plugins.loadOfx";
    let loader = s.ofx_loader.ok_or_else(|| EngineError::Other(NO_OFX.into()))?;
    let path =
        str_p(p, "path").map(str::trim).filter(|p| !p.is_empty()).ok_or_else(|| bad(C, "need `path` (an .ofx file, an .ofx.bundle folder or a folder)"))?;
    if !std::path::Path::new(path).exists() {
        return Err(bad(C, format!("`{path}` does not exist")));
    }
    let r = loader(path).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let n = loaded_count(&r);
    if n > 0 {
        s.toast(format!("Loaded {n} OpenFX effect(s)"));
    }
    for e in r.get("errors").and_then(Value::as_array).into_iter().flatten() {
        s.log.push(crate::panels::Level::Warning, "OpenFX", e.as_str().map(str::to_string).unwrap_or_else(|| e.to_string()));
    }
    if n == 0 && r.get("errors").and_then(Value::as_array).is_some_and(|e| !e.is_empty()) {
        s.error_toast("OpenFX", format!("No OpenFX effects loaded from {path} (see the Events panel)"));
    }
    Ok(r)
}

/// `effects.plugins.scanOfx`: load every plug-in in the host's OpenFX search paths
/// ([`Session::ofx_search_paths`]). Missing folders are skipped; one bad plug-in doesn't stop the
/// rest. Plug-ins that crashed the app while loading last time are skipped (the host keeps a
/// blocklist); `retry: true` forgets that list first. Returns `{paths, loaded, errors}`.
fn scan_ofx(s: &mut Session, p: &Value) -> Result<Value> {
    let loader = s.ofx_loader.ok_or_else(|| EngineError::Other(NO_OFX.into()))?;
    if bool_p(p, "retry").unwrap_or(false)
        && let Some(clear) = s.ofx_clear_blocklist
    {
        clear();
    }
    let (mut paths, mut loaded, mut errors) = (vec![], vec![], vec![]);
    for dir in s.ofx_search_paths.clone() {
        if !dir.exists() {
            continue;
        }
        let path = dir.to_string_lossy().to_string();
        match loader(&path) {
            Ok(r) => {
                loaded.extend(r.get("loaded").and_then(Value::as_array).cloned().unwrap_or_default());
                errors.extend(r.get("errors").and_then(Value::as_array).cloned().unwrap_or_default());
            }
            Err(e) => errors.push(json!(format!("{path}: {e}"))),
        }
        paths.push(path);
    }
    Ok(json!({"paths": paths, "loaded": loaded, "errors": errors}))
}

pub fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "effects.plugins.list",
            label: "List Effect Plug-ins",
            menu: &[],
            shortcut: None,
            params: "{} → {api, ofx, plugins: [{id, name, category, source, params}]}",
            enabled: always,
            run: list,
            journal: false,
        },
        CommandSpec {
            id: "effects.plugins.loadOfx",
            label: "Load OpenFX Plug-in…",
            menu: &["File"],
            shortcut: None,
            params: r#"{"path":str (.ofx binary | .ofx.bundle folder | a folder, scanned recursively)} → {loaded: [{id, name, category, ofxId, context, bundle}], errors}"#,
            enabled: has_ofx,
            run: load_ofx,
            journal: true,
        },
        CommandSpec {
            id: "effects.plugins.scanOfx",
            label: "Scan for OpenFX Plug-ins",
            menu: &[],
            shortcut: None,
            params: r#"{"retry":bool?} → {paths, loaded, errors}: loads every OpenFX plug-in in the standard folders and OFX_PLUGIN_PATH; plug-ins that crashed the app while loading last time are skipped unless retry is true"#,
            enabled: has_ofx,
            run: scan_ofx,
            journal: true,
        },
    ]
}
