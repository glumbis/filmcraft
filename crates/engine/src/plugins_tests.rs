//! Plug-in effects through the engine: listing, applying by id and name from the Effects panel's
//! command, rendering, and the OpenFX commands with hostile parameters (no `filmcraft-ofx` here:
//! the OFX loader is a stand-in).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use filmcraft_project::plugin::{EffectPlugin, PluginFrame, PluginManifest, PluginParam, PluginParamKind, PluginParams, register_plugin};
use serde_json::{Value, json};

use crate::{Event, Session};

/// Inverts colours in the encoding plug-ins see (sRGB, premultiplied).
struct Invert(PluginManifest);
impl EffectPlugin for Invert {
    fn manifest(&self) -> &PluginManifest {
        &self.0
    }
    fn render(&self, frame: &mut PluginFrame, _: &PluginParams, _: f64) -> Result<(), String> {
        for p in frame.pixels.iter_mut() {
            *p = [p[3] - p[0], p[3] - p[1], p[3] - p[2], p[3]];
        }
        Ok(())
    }
    fn source(&self) -> String {
        "engine test".into()
    }
}

fn invert(id: &str, name: &str) {
    let m = PluginManifest {
        api: 2,
        id: id.into(),
        name: name.into(),
        category: "Engine Tests".into(),
        version: String::new(),
        author: String::new(),
        description: String::new(),
        params: vec![PluginParam { id: "center".into(), name: "Center".into(), group: String::new(), kind: PluginParamKind::Point { default: [0.25, 0.75] } }],
    };
    register_plugin(Arc::new(Invert(m))).unwrap();
}

fn matte_sequence(hex: &str) -> (Session, u64) {
    let mut s = Session::default();
    let id = s.execute("file.newColorMatte", json!({"color": hex, "seconds": 2, "width": 64, "height": 36})).unwrap()["item"].as_u64().unwrap();
    s.execute("file.newSequenceFromClip", json!({"items": [id]})).unwrap();
    let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    (s, clip)
}

fn centre_rgb(s: &Session) -> [u8; 3] {
    let img = s.render_program(1.0).unwrap();
    let px = img.to_rgba8();
    let i = ((img.h / 2) * img.w + img.w / 2) * 4;
    [px[i], px[i + 1], px[i + 2]]
}

#[test]
fn a_plugin_effect_is_listed_applied_and_rendered() {
    invert("org.fctest.engine.invert", "Engine Test Invert");
    let (mut s, clip) = matte_sequence("#c86432");
    assert_eq!(centre_rgb(&s), [200, 100, 50]);

    // listed with the built-ins, in its own folder under Video Effects
    let list = s.execute("effects.list", json!({"folder": "Video Effects/Engine Tests"})).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1, "{list}");
    assert_eq!(list[0]["id"], "org.fctest.engine.invert");
    assert_eq!(list[0]["category"], json!(["Video Effects", "Engine Tests"]));
    let plugins = s.execute("effects.plugins.list", json!({})).unwrap();
    let me = plugins["plugins"].as_array().unwrap().iter().find(|p| p["id"] == "org.fctest.engine.invert").cloned().unwrap();
    assert_eq!(me["source"], "engine test");
    assert_eq!(me["params"], json!(["center"]));
    assert_eq!(plugins["ofx"], false);

    // applied by name (the Effects panel applies by id), its auto point resolved against the frame
    s.execute("effects.apply", json!({"clips": [clip], "effect": "engine test invert"})).unwrap();
    let item = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
    let inst = item.effects.iter().find(|e| e.effect == "org.fctest.engine.invert").unwrap();
    let c = inst.params["center"].value.as_vec2().unwrap();
    assert!(c.x.is_finite() && c.y.is_finite(), "auto point resolved: {c:?}");
    assert_eq!(centre_rgb(&s), [55, 155, 205], "inverted in sRGB");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(centre_rgb(&s), [200, 100, 50]);
    s.execute("effects.apply", json!({"clips": [clip], "effect": "org.fctest.engine.invert"})).unwrap();
    assert_eq!(centre_rgb(&s), [55, 155, 205]);
    // two of them cancel out
    s.execute("effects.apply", json!({"clips": [clip], "effect": "org.fctest.engine.invert"})).unwrap();
    assert_eq!(centre_rgb(&s), [200, 100, 50]);
}

#[test]
fn ofx_commands_need_a_loader() {
    let mut s = Session::default();
    for (id, p) in [("effects.plugins.loadOfx", json!({"path": "C:/x.ofx"})), ("effects.plugins.scanOfx", json!({}))] {
        assert!(!s.is_enabled(id));
        let e = s.execute(id, p).unwrap_err().to_string();
        assert!(e.contains("not available in this build"), "{id}: {e}");
    }
    assert_eq!(s.execute("effects.plugins.list", json!({})).unwrap()["ofx"], false);
    let menu = crate::commands::find("effects.plugins.loadOfx").unwrap();
    assert_eq!((menu.label, menu.menu), ("Load OpenFX Plug-in…", &["File"][..]));
}

static LOADS: AtomicUsize = AtomicUsize::new(0);
static CLEARS: AtomicUsize = AtomicUsize::new(0);

/// A stand-in OFX loader: `*.ofx` "loads" one effect, `fail` errors, anything else loads nothing.
fn fake_loader(path: &str) -> Result<Value, String> {
    LOADS.fetch_add(1, Ordering::SeqCst);
    if path.ends_with("fail") {
        return Err("the loader failed".into());
    }
    if path.ends_with(".ofx") {
        return Ok(
            json!({"loaded": [{"id": "org.fctest.fake", "name": "Fake", "category": "Fake", "ofxId": "org.fctest.fake", "context": "filter", "bundle": path}], "errors": []}),
        );
    }
    Ok(json!({"loaded": [], "errors": [format!("{path}: no plug-ins")]}))
}

fn fake_clear() {
    CLEARS.fetch_add(1, Ordering::SeqCst);
}

#[test]
fn ofx_commands_survive_hostile_parameters() {
    let dir = std::env::temp_dir().join(format!("fc-ofx-cmd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ofx = dir.join("thing.ofx");
    std::fs::write(&ofx, b"not a plug-in").unwrap();
    let fail = dir.join("fail");
    std::fs::create_dir_all(&fail).unwrap();

    let mut s = Session { ofx_loader: Some(fake_loader), ofx_clear_blocklist: Some(fake_clear), ..Default::default() };
    assert!(s.is_enabled("effects.plugins.loadOfx") && s.is_enabled("effects.plugins.scanOfx"));
    assert_eq!(s.execute("effects.plugins.list", json!({})).unwrap()["ofx"], true);
    for p in [
        json!({}),
        json!({"path": ""}),
        json!({"path": "   "}),
        json!({"path": 42}),
        json!({"path": null}),
        json!({"path": ["a"]}),
        json!({"path": {"x": 1}}),
        json!({"path": "Z:/definitely/not/here.ofx"}),
        json!({"path": "\u{0}garbage\u{fffd}"}),
        json!("not an object"),
        json!(null),
    ] {
        let before = LOADS.load(Ordering::SeqCst);
        assert!(s.execute("effects.plugins.loadOfx", p.clone()).is_err(), "{p}");
        assert_eq!(LOADS.load(Ordering::SeqCst), before, "{p}: the loader is not called");
    }
    // a loader error names the path
    let e = s.execute("effects.plugins.loadOfx", json!({"path": fail.to_string_lossy()})).unwrap_err().to_string();
    assert!(e.contains("the loader failed") && e.contains("fail"), "{e}");
    // a load: the result comes back and the user is told
    s.events.clear();
    let r = s.execute("effects.plugins.loadOfx", json!({"path": ofx.to_string_lossy()})).unwrap();
    assert_eq!(r["loaded"][0]["id"], "org.fctest.fake");
    assert!(s.events.iter().any(|e| matches!(e, Event::Toast { message, error: false } if message == "Loaded 1 OpenFX effect(s)")), "{:?}", s.events);
    // nothing loaded, errors: an error toast
    s.events.clear();
    let r = s.execute("effects.plugins.loadOfx", json!({"path": dir.to_string_lossy()})).unwrap();
    assert_eq!(r["loaded"], json!([]));
    assert!(s.events.iter().any(|e| matches!(e, Event::Toast { error: true, .. })));

    // scan: missing folders are skipped, a failing one becomes an error, retry clears the blocklist
    s.ofx_search_paths = vec![dir.join("missing"), fail.clone(), dir.clone()];
    let clears = CLEARS.load(Ordering::SeqCst);
    let r = s.execute("effects.plugins.scanOfx", json!({"retry": "yes"})).unwrap();
    assert_eq!(CLEARS.load(Ordering::SeqCst), clears, "retry must be true");
    assert_eq!(r["paths"].as_array().unwrap().len(), 2);
    let errors: Vec<&str> = r["errors"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(errors.iter().any(|e| e.contains("the loader failed")), "{errors:?}");
    s.execute("effects.plugins.scanOfx", json!({"retry": true})).unwrap();
    assert_eq!(CLEARS.load(Ordering::SeqCst), clears + 1);
    for p in [json!(null), json!([1, 2]), json!({"retry": null}), json!({"retry": 1e308})] {
        assert!(s.execute("effects.plugins.scanOfx", p.clone()).is_ok(), "{p}");
    }
    s.ofx_search_paths.clear();
    assert_eq!(s.execute("effects.plugins.scanOfx", json!({})).unwrap(), json!({"paths": [], "loaded": [], "errors": []}));
    std::fs::remove_dir_all(dir).unwrap();
}
