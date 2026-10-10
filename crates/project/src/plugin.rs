//! The native effect plug-in API, version [`PLUGIN_API_VERSION`].
//!
//! A plug-in is anything implementing [`EffectPlugin`]: a [`PluginManifest`] (stable id, display
//! name, Effects-panel folder, typed parameters) and a `render` that processes a [`PluginFrame`]
//! in place. Registering one turns it into an ordinary [`crate::EffectDef`] (Video Effects ▸ its
//! folder), so its parameters keyframe, show in Effect Controls and are addressable by agents,
//! and it is applied by id like a built-in. The OpenFX host (`filmcraft-ofx`) is built on it; the
//! API is the one EffectCraft uses (same types, same contract), so hosts port between the two.
//!
//! The contract (stable within an API version):
//! * pixels are premultiplied RGBA `f32`, **sRGB-encoded** (display referred, may exceed 1),
//!   row-major, row 0 on top, in the clip's working image, which may be scaled (preview
//!   resolution) and padded;
//! * parameters are flattened to `f64`s in declaration order: slider / angle / checkbox (0 or 1)
//!   / popup (0-based index) take one value, point two (**buffer pixels**), colour four (RGBA
//!   0–1), point3 three (x, y like a point, z as is), layer one (a video track index, -1 for
//!   none); text / hidden strings take no slot and are read with [`PluginParams::text`];
//! * `time` is clip time in seconds (0 = the clip's first frame); `scale` is buffer pixels per
//!   source pixel (scale pixel distances like blur radii by it);
//! * rendering must be deterministic and may run on several threads at once.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use filmcraft_geom::Vec2;
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};

use crate::effect::{EffectDef, EffectInstance, EffectKind, ParamDef, ParamKind, TRACK_CHOICES};
use crate::keyframe::ParamValue;

pub const PLUGIN_API_VERSION: u32 = 2;

/// The most plug-in effects one process can register (Sapphire alone registers ~300).
pub const MAX_PLUGINS: usize = 1024;

/// The most transparent pixels a plug-in may ask [`EffectPlugin::padding`] to add per side.
pub const MAX_PADDING: u32 = 4096;

/// A plug-in parameter's kind, default and range.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PluginParamKind {
    Slider {
        default: f64,
        #[serde(default)]
        min: f64,
        #[serde(default = "hundred")]
        max: f64,
        /// Slider range (defaults to min..max).
        #[serde(default, rename = "sliderMin")]
        slider_min: Option<f64>,
        #[serde(default, rename = "sliderMax")]
        slider_max: Option<f64>,
        #[serde(default = "two")]
        decimals: u8,
        /// Whole numbers (API 2; OpenFX int parameters): shown with 0 decimals.
        #[serde(default)]
        integer: bool,
    },
    Angle {
        #[serde(default)]
        default: f64,
    },
    Checkbox {
        #[serde(default)]
        default: bool,
    },
    Popup {
        options: Vec<String>,
        /// 0-based index.
        #[serde(default)]
        default: u32,
    },
    /// Default as fractions of the layer size (`[0.5, 0.5]` = centre).
    Point {
        #[serde(default = "centre")]
        default: [f64; 2],
    },
    /// RGBA, 0–1.
    Color {
        #[serde(default = "white")]
        default: [f64; 4],
    },
    /// API 2: a 3D point (x, y in frame pixels like [`PluginParamKind::Point`], z as is). The
    /// default is in layer pixels (not fractions).
    Point3 {
        #[serde(default)]
        default: [f64; 3],
    },
    /// API 2: another layer of the composition (flattens to its id, -1 for none).
    Layer,
    /// API 2: a string, shown as a text field; read with [`PluginParams::text`].
    Text {
        #[serde(default)]
        default: String,
    },
    /// API 2: a string that is not shown (opaque plug-in data); read with [`PluginParams::text`].
    Hidden {
        #[serde(default)]
        default: String,
    },
}

fn hundred() -> f64 {
    100.0
}
fn two() -> u8 {
    2
}
fn centre() -> [f64; 2] {
    [0.5, 0.5]
}
fn white() -> [f64; 4] {
    [1.0; 4]
}

impl PluginParamKind {
    /// How many `f64`s the parameter flattens to.
    pub fn width(&self) -> usize {
        match self {
            PluginParamKind::Point { .. } => 2,
            PluginParamKind::Point3 { .. } => 3,
            PluginParamKind::Color { .. } => 4,
            PluginParamKind::Text { .. } | PluginParamKind::Hidden { .. } => 0,
            _ => 1,
        }
    }
    /// Whether the parameter holds a string (it flattens to no `f64`s but one entry of
    /// [`PluginParams::texts`]).
    pub fn is_text(&self) -> bool {
        matches!(self, PluginParamKind::Text { .. } | PluginParamKind::Hidden { .. })
    }
    /// Whether the kind needs plug-in API 2 (WebAssembly plug-ins can't carry it).
    pub fn needs_v2(&self) -> bool {
        matches!(self, PluginParamKind::Point3 { .. } | PluginParamKind::Layer | PluginParamKind::Text { .. } | PluginParamKind::Hidden { .. })
    }
}

/// One parameter of a plug-in effect.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginParam {
    /// Stable id (the property's match name; letters, digits, `_`).
    pub id: String,
    pub name: String,
    /// API 2: the group / page path the parameter belongs to (`Advanced/Edges`). Hosts may
    /// ignore it.
    #[serde(default)]
    pub group: String,
    #[serde(flatten)]
    pub kind: PluginParamKind,
}

/// What a plug-in declares about itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginManifest {
    /// The plug-in API version it was written for (1, or 2 up to [`PLUGIN_API_VERSION`]).
    pub api: u32,
    /// Stable, reverse-domain effect id (`org.example.posterize`); `fc.` is reserved.
    pub id: String,
    pub name: String,
    /// Effects & Presets category: the Effects-panel folder under Video Effects.
    pub category: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub params: Vec<PluginParam>,
}

impl PluginManifest {
    /// Check the manifest against this API version.
    pub fn validate(&self) -> Result<(), String> {
        // API 2 (OpenFX) effects such as Sapphire's can declare far more parameters.
        let max_params: usize = if self.api >= 2 { 1024 } else { 256 };
        // Opaque plug-in data can be large (Sapphire's S_ColorFuse ships a LUT of a few MB).
        const MAX_TEXT: usize = 64 << 20;
        if !(1..=PLUGIN_API_VERSION).contains(&self.api) {
            return Err(format!("plug-in `{}` targets plug-in API {} but this build implements API 1 to {PLUGIN_API_VERSION}", self.id, self.api));
        }
        let ok_id = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
        if !ok_id(&self.id) || self.id.starts_with("fc.") {
            return Err(format!("bad plug-in id `{}` (reverse-domain, letters, digits, `.`, `_`, `-`; `fc.` is reserved)", self.id));
        }
        if self.name.trim().is_empty() || self.category.trim().is_empty() {
            return Err(format!("plug-in `{}` needs a name and a category", self.id));
        }
        if self.params.len() > max_params {
            return Err(format!("plug-in `{}` has {} parameters (at most {max_params})", self.id, self.params.len()));
        }
        let mut seen = std::collections::HashSet::new();
        for p in &self.params {
            if p.id.is_empty() || !p.id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || !seen.insert(p.id.as_str()) {
                return Err(format!("plug-in `{}`: bad or duplicate parameter id `{}`", self.id, p.id));
            }
            if let PluginParamKind::Popup { options, default } = &p.kind
                && (options.is_empty() || *default as usize >= options.len())
            {
                return Err(format!("plug-in `{}`: popup `{}` needs options and a default index in range", self.id, p.id));
            }
            if let PluginParamKind::Point3 { default } = &p.kind
                && default.iter().any(|v| !v.is_finite())
            {
                return Err(format!("plug-in `{}`: point3 `{}` needs a finite default", self.id, p.id));
            }
            if let PluginParamKind::Text { default } | PluginParamKind::Hidden { default } = &p.kind
                && default.len() > MAX_TEXT
            {
                return Err(format!("plug-in `{}`: the default of `{}` is longer than {MAX_TEXT} bytes", self.id, p.id));
            }
            if let PluginParamKind::Slider { default, min, max, slider_min, slider_max, .. } = &p.kind {
                let (lo, hi) = (slider_min.unwrap_or(*min), slider_max.unwrap_or(*max));
                if [*default, *min, *max, lo, hi].iter().any(|v| !v.is_finite()) || min > max || lo > hi || default < min || default > max {
                    return Err(format!("plug-in `{}`: slider `{}` needs min ≤ default ≤ max (and sliderMin ≤ sliderMax)", self.id, p.id));
                }
            }
        }
        Ok(())
    }

    /// Total flattened parameter count.
    pub fn flat_len(&self) -> usize {
        self.params.iter().map(|p| p.kind.width()).sum()
    }
}

/// The pixels a plug-in renders, in place.
pub struct PluginFrame<'a> {
    pub width: u32,
    pub height: u32,
    /// Premultiplied RGBA, row-major (row 0 = top), `width * height` pixels.
    pub pixels: &'a mut [[f32; 4]],
    /// Buffer pixels per layer pixel (preview resolution).
    pub scale: f64,
    /// API 2: buffer-pixel position of layer pixel (0, 0) (padding shifts it).
    pub origin: [f64; 2],
    /// API 2: the layer's bounds in layer pixels.
    pub layer_size: [f64; 2],
}

/// An image the host hands an API 2 plug-in (the input at another time, or another layer).
/// Same layout as [`PluginFrame`]: premultiplied RGBA, row 0 = top, `origin` = buffer pixels of
/// layer pixel (0, 0) in this image.
#[derive(Clone, Debug, Default)]
pub struct PluginImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<[f32; 4]>,
    pub origin: [f64; 2],
    pub scale: f64,
}

/// Services available during an API 2 render ([`EffectPlugin::render_v2`]).
pub trait PluginHost {
    /// This effect's input (its layer with masks and the effects above this one) at another
    /// **layer time** in seconds, in its own extent (see [`PluginImage::origin`]; it is not
    /// padded). `None` when unavailable (adjustment layers, no host).
    fn input_at(&self, layer_time: f64) -> Option<PluginImage>;
    /// The layer chosen in Layer parameter `param_id` at **layer time** `layer_time` (converted
    /// to composition time), effects and masks applied, resampled into this layer's buffer
    /// (same size, origin and scale as the frame, centred when the layers differ in size).
    /// `None` when unset or unavailable.
    fn layer_input(&self, param_id: &str, layer_time: f64) -> Option<PluginImage>;
    /// All flattened parameter values (and texts) at another layer time.
    fn params_at(&self, layer_time: f64) -> Option<(Vec<f64>, Vec<String>)>;
    /// Composition frame rate (30 when unknown).
    fn frame_rate(&self) -> f64;
    /// Layer time of the current render, seconds.
    fn time(&self) -> f64;
    /// A stable id of the applied effect instance: the same on every frame of one effect on one
    /// layer, different for every layer and every application of the effect. Plug-ins that keep
    /// state between frames pool their native instances by it. 0 when unknown.
    fn instance_key(&self) -> u64 {
        0
    }
    /// The layer's source time range in **layer time** seconds as (first, end): the first source
    /// frame starts at `first` and the source ends at `end` .
    /// `None` when the source has no duration or the host does not know it (retimers then
    /// assume an unbounded range).
    fn frame_range(&self) -> Option<(f64, f64)> {
        None
    }
}

/// Parameter values for one render, flattened (see the module docs) with typed accessors.
pub struct PluginParams<'a> {
    pub manifest: &'a PluginManifest,
    pub flat: &'a [f64],
    /// One string per text / hidden parameter, in declaration order.
    pub texts: &'a [String],
}

impl PluginParams<'_> {
    fn slot(&self, id: &str) -> Option<&[f64]> {
        let mut at = 0;
        for p in &self.manifest.params {
            let w = p.kind.width();
            if p.id == id {
                return self.flat.get(at..at + w);
            }
            at += w;
        }
        None
    }
    /// Slider / angle / popup index / checkbox (0 or 1).
    pub fn f(&self, id: &str) -> f64 {
        self.slot(id).and_then(|s| s.first().copied()).unwrap_or(0.0)
    }
    pub fn b(&self, id: &str) -> bool {
        self.f(id) != 0.0
    }
    /// Point in frame pixels.
    pub fn point(&self, id: &str) -> [f64; 2] {
        self.slot(id).and_then(|s| Some([*s.first()?, *s.get(1)?])).unwrap_or([0.0; 2])
    }
    /// 3D point: x, y in frame pixels, z as is.
    pub fn point3(&self, id: &str) -> [f64; 3] {
        self.slot(id).and_then(|s| Some([*s.first()?, *s.get(1)?, *s.get(2)?])).unwrap_or([0.0; 3])
    }
    pub fn color(&self, id: &str) -> [f64; 4] {
        self.slot(id).and_then(|s| Some([*s.first()?, *s.get(1)?, *s.get(2)?, *s.get(3)?])).unwrap_or([1.0; 4])
    }
    /// The layer chosen in a layer parameter.
    pub fn layer(&self, id: &str) -> Option<u64> {
        self.slot(id).and_then(|s| s.first().copied()).filter(|v| *v >= 0.0).map(|v| v as u64)
    }
    /// A text / hidden parameter's string (empty for other ids).
    pub fn text(&self, id: &str) -> &str {
        let mut at = 0;
        for p in &self.manifest.params {
            if p.kind.is_text() {
                if p.id == id {
                    return self.texts.get(at).map(String::as_str).unwrap_or("");
                }
                at += 1;
            }
        }
        ""
    }
}

/// An effect plug-in.
pub trait EffectPlugin: Send + Sync {
    fn manifest(&self) -> &PluginManifest;
    /// Process `frame` in place. `time` is layer time in seconds. An error leaves the frame as
    /// it was (the error is logged once).
    fn render(&self, frame: &mut PluginFrame, params: &PluginParams, time: f64) -> Result<(), String>;
    /// Where the plug-in came from (a file path, `built-in`…), for listings.
    fn source(&self) -> String {
        "built-in".into()
    }
    /// API 2: transparent pixels to add on every side of the buffer before rendering (glows,
    /// blurs, regions of definition larger than the input), in buffer pixels. `layer_size` is
    /// the layer's size in layer pixels (the frame's [`PluginFrame::layer_size`]; 0 when unknown)
    /// and `instance_key` the [`PluginHost::instance_key`] this render will have.
    fn padding(&self, _params: &PluginParams, _time: f64, _scale: f64, _layer_size: [f64; 2], _instance_key: u64) -> u32 {
        0
    }
    /// API 2: [`EffectPlugin::render`] with host services. The default calls `render`. May be
    /// called from several threads at once.
    fn render_v2(&self, frame: &mut PluginFrame, params: &PluginParams, time: f64, _host: &dyn PluginHost) -> Result<(), String> {
        self.render(frame, params, time)
    }
}

// ---- registry ----

/// One registered plug-in.
struct Registered {
    plugin: Arc<dyn EffectPlugin>,
    def: &'static EffectDef,
    /// Point (and auto Point3) parameters' default positions as fractions of the frame, for
    /// [`crate::effect::auto_point`].
    points: Vec<(&'static str, (f64, f64))>,
}

#[derive(Default)]
struct Registry {
    /// In registration order.
    list: Vec<Registered>,
    by_id: HashMap<&'static str, usize>,
}

fn registry() -> &'static RwLock<Registry> {
    static REGISTRY: OnceLock<RwLock<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

/// Run `f` on the registry under the read lock (a poisoned lock is still readable).
fn read<T>(f: impl FnOnce(&Registry) -> T) -> T {
    let r = registry().read().unwrap_or_else(PoisonError::into_inner);
    f(&r)
}

/// A `'static` copy of `s`. Registered definitions live for the whole process and there are at
/// most [`MAX_PLUGINS`] of them, so leaking their strings is bounded.
fn leak(s: &str) -> &'static str {
    Box::leak(s.to_owned().into_boxed_str())
}

/// `v` when finite, else `fallback`.
fn finite_or(v: f64, fallback: f64) -> f64 {
    if v.is_finite() { v } else { fallback }
}

/// The id of the Float parameter holding the z of Point3 parameter `param_id` (`<id>_z`).
/// A manifest that already declares that id gets another one: see [`point3_z_id_for`].
pub fn point3_z_id(param_id: &str) -> String {
    format!("{param_id}_z")
}

/// The id of the z parameter of Point3 parameter `param_id` of manifest `m`: [`point3_z_id`],
/// or `<id>_z2`, `<id>_z3`… when the manifest declares a parameter with that id. Never equal to
/// a manifest parameter id or to the z id of another Point3 parameter.
pub fn point3_z_id_for(m: &PluginManifest, param_id: &str) -> String {
    let taken = |id: &str| m.params.iter().any(|p| p.id == id);
    let first = point3_z_id(param_id);
    if !taken(&first) {
        return first;
    }
    // `params.len() + 1` distinct candidates: at most `params.len()` of them can be taken
    let last = m.params.len().saturating_add(2);
    (2..=last).map(|n| format!("{param_id}_z{n}")).find(|id| !taken(id)).unwrap_or(first)
}

/// The default of a Point parameter as fractions of the frame (centre for non-finite values).
fn point_fraction(default: [f64; 2]) -> (f64, f64) {
    (finite_or(default[0], 0.5), finite_or(default[1], 0.5))
}

/// Whether a Point3 default leaves x, y "auto" (the frame centre): a `(0, 0)` default.
fn point3_auto(default: [f64; 3]) -> bool {
    default[0] == 0.0 && default[1] == 0.0
}

/// Build the effect definition of a (validated) manifest, with the point fractions for
/// [`crate::effect::auto_point`].
fn build_def(m: &PluginManifest) -> (EffectDef, Vec<(&'static str, (f64, f64))>) {
    let mut params = Vec::with_capacity(m.params.len());
    let mut points = Vec::new();
    for p in &m.params {
        let id = leak(&p.id);
        let label = leak(&p.name);
        let group = if p.group.is_empty() { None } else { Some(leak(&p.group)) };
        let def = |kind: ParamKind, default: ParamValue, animatable: bool| ParamDef { id, label, kind, default, animatable, group };
        match &p.kind {
            PluginParamKind::Slider { default, min, max, slider_min, slider_max, decimals, integer } => {
                let kind = ParamKind::Float {
                    min: *min,
                    max: *max,
                    soft_min: slider_min.unwrap_or(*min),
                    soft_max: slider_max.unwrap_or(*max),
                    unit: "",
                    decimals: if *integer { 0 } else { *decimals },
                };
                params.push(def(kind, ParamValue::Float(*default), true));
            }
            PluginParamKind::Angle { default } => params.push(def(ParamKind::Angle, ParamValue::Float(finite_or(*default, 0.0)), true)),
            PluginParamKind::Checkbox { default } => params.push(def(ParamKind::Bool, ParamValue::Bool(*default), false)),
            PluginParamKind::Popup { options, default } => {
                let opts: Vec<&'static str> = options.iter().map(|o| leak(o)).collect();
                let opts: &'static [&'static str] = Box::leak(opts.into_boxed_slice());
                params.push(def(ParamKind::Choice(opts), ParamValue::Choice(*default), false));
            }
            PluginParamKind::Point { default } => {
                points.push((id, point_fraction(*default)));
                params.push(def(ParamKind::Point, ParamValue::Vec2(Vec2::new(f64::NAN, f64::NAN)), true));
            }
            PluginParamKind::Color { default } => {
                let c = default.map(|v| finite_or(v, 1.0) as f32);
                params.push(def(ParamKind::Color, ParamValue::Color(c), true));
            }
            PluginParamKind::Point3 { default } => {
                let xy = if point3_auto(*default) {
                    points.push((id, (0.5, 0.5)));
                    Vec2::new(f64::NAN, f64::NAN)
                } else {
                    Vec2::new(default[0], default[1])
                };
                params.push(def(ParamKind::Point, ParamValue::Vec2(xy), true));
                params.push(ParamDef {
                    id: leak(&point3_z_id_for(m, &p.id)),
                    label: leak(&format!("{} Z", p.name)),
                    kind: ParamKind::Float { min: -1e6, max: 1e6, soft_min: -1000.0, soft_max: 1000.0, unit: "", decimals: 2 },
                    default: ParamValue::Float(default[2]),
                    animatable: true,
                    group,
                });
            }
            PluginParamKind::Layer => params.push(def(ParamKind::Choice(TRACK_CHOICES), ParamValue::Choice(0), false)),
            PluginParamKind::Text { default } => params.push(def(ParamKind::Text, ParamValue::Text(default.clone()), false)),
            PluginParamKind::Hidden { default } => params.push(def(ParamKind::Hidden, ParamValue::Text(default.clone()), false)),
        }
    }
    let category: &'static [&'static str] = Box::leak(vec!["Video Effects", leak(&m.category)].into_boxed_slice());
    let def = EffectDef {
        id: leak(&m.id),
        name: leak(&m.name),
        kind: EffectKind::Video,
        category,
        params,
        intrinsic: false,
        accelerated: false,
        float32: true,
        yuv: false,
    };
    (def, points)
}

/// Register a plug-in effect as a Video Effect (Effects panel: Video Effects ▸ its category).
/// Fails for a bad manifest, an id taken by a built-in effect or another plug-in, or when
/// [`MAX_PLUGINS`] are registered. The definition lives for the rest of the process.
pub fn register_plugin(plugin: Arc<dyn EffectPlugin>) -> Result<&'static EffectDef, String> {
    let m = plugin.manifest();
    m.validate()?;
    // checked before taking the lock (building the built-ins must never wait on the registry)
    if crate::effect::effect_defs().iter().any(|e| e.id == m.id) {
        return Err(format!("plug-in id `{}` is taken by a built-in effect", m.id));
    }
    let mut r = registry().write().unwrap_or_else(PoisonError::into_inner);
    if r.by_id.contains_key(m.id.as_str()) {
        return Err(format!("a plug-in with id `{}` is already registered", m.id));
    }
    if r.list.len() >= MAX_PLUGINS {
        return Err(format!("cannot register plug-in `{}`: at most {MAX_PLUGINS} plug-in effects can be registered", m.id));
    }
    let (def, points) = build_def(m);
    let def: &'static EffectDef = Box::leak(Box::new(def));
    let at = r.list.len();
    r.by_id.insert(def.id, at);
    r.list.push(Registered { plugin: Arc::clone(&plugin), def, points });
    Ok(def)
}

/// The plug-in behind a registered effect id.
pub fn plugin(id: &str) -> Option<Arc<dyn EffectPlugin>> {
    read(|r| r.by_id.get(id).and_then(|&i| r.list.get(i)).map(|e| Arc::clone(&e.plugin)))
}

/// The effect definition of a registered plug-in (built-ins are not searched; use
/// [`crate::find_effect`] for both).
pub fn plugin_def(id: &str) -> Option<&'static EffectDef> {
    read(|r| r.by_id.get(id).and_then(|&i| r.list.get(i)).map(|e| e.def))
}

/// The definitions of every registered plug-in, in registration order.
pub fn plugin_defs() -> Vec<&'static EffectDef> {
    read(|r| r.list.iter().map(|e| e.def).collect())
}

/// The default position (fractions of the frame) of a registered plug-in's auto point.
pub(crate) fn plugin_auto_point(effect: &str, param: &str) -> Option<(f64, f64)> {
    read(|r| {
        let e = r.list.get(*r.by_id.get(effect)?)?;
        e.points.iter().find(|(id, _)| *id == param).map(|(_, f)| *f)
    })
}

/// A scalar parameter's finite value at `t` (Float, Choice or Bool), if present.
fn scalar(e: &EffectInstance, id: &str, t: Tick) -> Option<f64> {
    e.params.get(id).and_then(|p| p.value_at(t).as_f64()).filter(|v| v.is_finite())
}

/// Flatten an applied plug-in effect's parameters at clip time `t` (ticks from the clip's
/// first frame, like keyframe times) into the `f64`s [`PluginParams::flat`] expects, in
/// declaration order (`m.flat_len()` values; see the module docs).
///
/// Points are stored in source (layer) pixels and mapped to buffer pixels as
/// `x * scale + origin[0]`, `y * scale + origin[1]`; an auto (NaN) or missing point falls back to
/// its default (fractions × `layer_size` for points, the layer centre for an auto point3).
/// Missing or mistyped parameters fall back to the manifest defaults; popup indices are clamped
/// to the options; a layer parameter gives the chosen track index − 1 (`-1` for none).
pub fn flatten(m: &PluginManifest, e: &EffectInstance, t: Tick, scale: f64, origin: [f64; 2], layer_size: [f64; 2]) -> Vec<f64> {
    let mut out = Vec::with_capacity(m.flat_len());
    let point_at = |id: &str| e.params.get(id).and_then(|p| p.value_at(t).as_vec2());
    let map = |v: Option<Vec2>, fallback: [f64; 2]| {
        let x = v.map(|v| v.x).filter(|x| x.is_finite()).unwrap_or(fallback[0]);
        let y = v.map(|v| v.y).filter(|y| y.is_finite()).unwrap_or(fallback[1]);
        [x * scale + origin[0], y * scale + origin[1]]
    };
    for p in &m.params {
        let id = p.id.as_str();
        match &p.kind {
            PluginParamKind::Slider { default, .. } | PluginParamKind::Angle { default } => {
                out.push(scalar(e, id, t).unwrap_or(finite_or(*default, 0.0)));
            }
            PluginParamKind::Checkbox { default } => {
                let on = scalar(e, id, t).map_or(*default, |v| v != 0.0);
                out.push(if on { 1.0 } else { 0.0 });
            }
            PluginParamKind::Popup { options, default } => {
                let last = options.len().saturating_sub(1) as f64;
                let idx = scalar(e, id, t).filter(|v| *v >= 0.0).map_or(*default as f64, f64::floor);
                out.push(idx.min(last).max(0.0));
            }
            PluginParamKind::Point { default } => {
                let (fx, fy) = point_fraction(*default);
                out.extend(map(point_at(id), [fx * layer_size[0], fy * layer_size[1]]));
            }
            PluginParamKind::Point3 { default } => {
                let fallback = if point3_auto(*default) { [layer_size[0] * 0.5, layer_size[1] * 0.5] } else { [default[0], default[1]] };
                out.extend(map(point_at(id), fallback));
                out.push(scalar(e, &point3_z_id_for(m, id), t).unwrap_or(finite_or(default[2], 0.0)));
            }
            PluginParamKind::Color { default } => {
                let c = e.params.get(id).and_then(|p| p.value_at(t).as_color());
                for (i, d) in default.iter().enumerate() {
                    let v = c.and_then(|c| c.get(i).copied()).map(f64::from).filter(|v| v.is_finite());
                    out.push(v.unwrap_or(finite_or(*d, 1.0)));
                }
            }
            PluginParamKind::Layer => {
                let last = TRACK_CHOICES.len().saturating_sub(1) as f64;
                let choice = scalar(e, id, t).filter(|v| *v >= 0.0).map_or(0.0, |v| v.floor().min(last));
                out.push(choice - 1.0);
            }
            PluginParamKind::Text { .. } | PluginParamKind::Hidden { .. } => {}
        }
    }
    out
}

/// The strings of an applied plug-in effect's text and hidden parameters, in declaration order
/// ([`PluginParams::texts`]); a missing or mistyped parameter gives its manifest default. Text
/// parameters are not animatable, so the static value is used.
pub fn flatten_texts(m: &PluginManifest, e: &EffectInstance) -> Vec<String> {
    m.params
        .iter()
        .filter_map(|p| match &p.kind {
            PluginParamKind::Text { default } | PluginParamKind::Hidden { default } => Some(match e.params.get(&p.id).map(|v| &v.value) {
                Some(ParamValue::Text(s)) => s.clone(),
                _ => default.clone(),
            }),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyframe::Keyframe;

    struct TestPlugin(PluginManifest);

    impl EffectPlugin for TestPlugin {
        fn manifest(&self) -> &PluginManifest {
            &self.0
        }
        fn render(&self, _frame: &mut PluginFrame, _params: &PluginParams, _time: f64) -> Result<(), String> {
            Ok(())
        }
    }

    fn param(id: &str, kind: PluginParamKind) -> PluginParam {
        PluginParam { id: id.into(), name: format!("Param {id}"), group: String::new(), kind }
    }

    /// A manifest with one parameter of every kind (plus a parameter named like point3 `p3`'s
    /// default z id, to exercise the collision rule).
    fn manifest(id: &str) -> PluginManifest {
        PluginManifest {
            api: 2,
            id: id.into(),
            name: format!("Test Plug-in {id}"),
            category: "Test Plug-ins".into(),
            version: "1.0".into(),
            author: String::new(),
            description: String::new(),
            params: vec![
                param(
                    "amount",
                    PluginParamKind::Slider { default: 25.0, min: 0.0, max: 200.0, slider_min: None, slider_max: Some(100.0), decimals: 1, integer: false },
                ),
                param("count", PluginParamKind::Slider { default: 3.0, min: 1.0, max: 10.0, slider_min: None, slider_max: None, decimals: 2, integer: true }),
                param("angle", PluginParamKind::Angle { default: 45.0 }),
                param("enable", PluginParamKind::Checkbox { default: true }),
                param("mode", PluginParamKind::Popup { options: vec!["A".into(), "B".into(), "C".into()], default: 1 }),
                param("center", PluginParamKind::Point { default: [0.25, 0.75] }),
                param("tint", PluginParamKind::Color { default: [1.0, 0.5, 0.25, 1.0] }),
                param("p3", PluginParamKind::Point3 { default: [0.0, 0.0, 7.0] }),
                param("p3_z", PluginParamKind::Slider { default: 1.0, min: 0.0, max: 2.0, slider_min: None, slider_max: None, decimals: 2, integer: false }),
                param("fixed3", PluginParamKind::Point3 { default: [10.0, 20.0, -3.0] }),
                param("other", PluginParamKind::Layer),
                param("label", PluginParamKind::Text { default: "hello".into() }),
                PluginParam {
                    id: "blob".into(),
                    name: "Blob".into(),
                    group: "Advanced/Data".into(),
                    kind: PluginParamKind::Hidden { default: "opaque".into() },
                },
            ],
        }
    }

    fn register(id: &str) -> &'static EffectDef {
        register_plugin(Arc::new(TestPlugin(manifest(id)))).unwrap()
    }

    #[test]
    fn registers_and_is_found_everywhere() {
        let def = register("org.test.registry_found");
        assert_eq!(def.id, "org.test.registry_found");
        assert_eq!(def.kind, EffectKind::Video);
        assert_eq!(def.category, &["Video Effects", "Test Plug-ins"]);
        assert!(!def.intrinsic && !def.accelerated && def.float32 && !def.yuv);
        assert!(std::ptr::eq(crate::find_effect("org.test.registry_found").unwrap(), def));
        assert!(std::ptr::eq(crate::effect::find_effect_by_name("test plug-in ORG.TEST.REGISTRY_FOUND").unwrap(), def));
        assert!(std::ptr::eq(crate::effect::find_effect_by_name("ORG.test.registry_found").unwrap(), def));
        assert!(crate::all_effect_defs().iter().any(|d| std::ptr::eq(*d, def)));
        assert!(plugin_defs().iter().any(|d| std::ptr::eq(*d, def)));
        assert!(!crate::effect_defs().iter().any(|d| d.id == def.id), "effect_defs stays built-in only");
        assert_eq!(plugin("org.test.registry_found").unwrap().manifest().id, "org.test.registry_found");
        assert!(plugin("org.test.not_registered").is_none());
        // built-ins first
        let all = crate::all_effect_defs();
        assert_eq!(all.first().map(|d| d.id), crate::effect_defs().first().map(|d| d.id));
        // an instance resolves its definition through the registry
        assert!(std::ptr::eq(def.instance().def().unwrap(), def));
    }

    #[test]
    fn parameters_map_to_param_defs() {
        let def = register("org.test.registry_params");
        let ids: Vec<&str> = def.params.iter().map(|p| p.id).collect();
        // point3 `p3` collides with the manifest's `p3_z`, so its z is `p3_z2`
        assert_eq!(
            ids,
            ["amount", "count", "angle", "enable", "mode", "center", "tint", "p3", "p3_z2", "p3_z", "fixed3", "fixed3_z", "other", "label", "blob"]
        );
        let p = |id: &str| def.param(id).unwrap();
        assert_eq!(p("amount").kind, ParamKind::Float { min: 0.0, max: 200.0, soft_min: 0.0, soft_max: 100.0, unit: "", decimals: 1 });
        assert_eq!(p("amount").default, ParamValue::Float(25.0));
        assert!(p("amount").animatable);
        assert!(matches!(p("count").kind, ParamKind::Float { decimals: 0, .. }));
        assert_eq!(p("angle").kind, ParamKind::Angle);
        assert_eq!(p("enable").kind, ParamKind::Bool);
        assert!(!p("enable").animatable);
        assert_eq!(p("mode").kind, ParamKind::Choice(&["A", "B", "C"]));
        assert_eq!(p("mode").default, ParamValue::Choice(1));
        assert_eq!(p("center").kind, ParamKind::Point);
        assert!(matches!(p("center").default, ParamValue::Vec2(v) if v.x.is_nan() && v.y.is_nan()));
        assert_eq!(p("tint").default, ParamValue::Color([1.0, 0.5, 0.25, 1.0]));
        assert!(matches!(p("p3").default, ParamValue::Vec2(v) if v.x.is_nan()));
        assert_eq!(p("p3_z2").label, "Param p3 Z");
        assert_eq!(p("p3_z2").default, ParamValue::Float(7.0));
        assert_eq!(p("fixed3").default, ParamValue::Vec2(Vec2::new(10.0, 20.0)));
        assert_eq!(p("fixed3_z").default, ParamValue::Float(-3.0));
        assert_eq!(p("other").kind, ParamKind::Choice(TRACK_CHOICES));
        assert_eq!(p("label").kind, ParamKind::Text);
        assert_eq!(p("label").default, ParamValue::Text("hello".into()));
        assert_eq!(p("blob").kind, ParamKind::Hidden);
        assert_eq!(p("blob").default, ParamValue::Text("opaque".into()));
        assert_eq!(p("blob").group, Some("Advanced/Data"));
        assert_eq!(p("amount").group, None);
        assert_eq!(point3_z_id("p3"), "p3_z");
        assert_eq!(point3_z_id_for(&manifest("x"), "p3"), "p3_z2");
        assert_eq!(point3_z_id_for(&manifest("x"), "fixed3"), "fixed3_z");
    }

    #[test]
    fn duplicate_builtin_and_bad_ids_are_rejected() {
        register("org.test.registry_dup");
        let again = register_plugin(Arc::new(TestPlugin(manifest("org.test.registry_dup"))));
        assert!(again.unwrap_err().contains("already registered"));
        let builtin = register_plugin(Arc::new(TestPlugin(manifest("gaussian_blur"))));
        assert!(builtin.unwrap_err().contains("built-in"));
        let reserved = register_plugin(Arc::new(TestPlugin(manifest("fc.reserved"))));
        assert!(reserved.is_err());
        assert!(crate::find_effect("fc.reserved").is_none());
        // the built-in keeps its definition
        assert_eq!(crate::find_effect("gaussian_blur").unwrap().name, "Gaussian Blur");
    }

    #[test]
    fn auto_points_resolve_to_default_fractions() {
        let def = register("org.test.registry_auto");
        assert_eq!(crate::effect::auto_point("org.test.registry_auto", "center"), Some((0.25, 0.75)));
        assert_eq!(crate::effect::auto_point("org.test.registry_auto", "p3"), Some((0.5, 0.5)));
        assert_eq!(crate::effect::auto_point("org.test.registry_auto", "amount"), None);
        let mut e = def.instance();
        crate::resolve_auto_points(&mut e, (1920, 1080), (1920, 1080));
        assert_eq!(e.vec2_at("center", Tick::ZERO), Vec2::new(480.0, 810.0));
        assert_eq!(e.vec2_at("p3", Tick::ZERO), Vec2::new(960.0, 540.0));
        assert_eq!(e.vec2_at("fixed3", Tick::ZERO), Vec2::new(10.0, 20.0));
    }

    #[test]
    fn flatten_follows_the_contract() {
        let def = register("org.test.registry_flatten");
        let m = manifest("org.test.registry_flatten");
        let mut e = def.instance();
        crate::resolve_auto_points(&mut e, (200, 100), (200, 100));
        let (scale, origin, layer) = (0.5, [10.0, 20.0], [200.0, 100.0]);
        let flat = flatten(&m, &e, Tick::ZERO, scale, origin, layer);
        assert_eq!(flat.len(), m.flat_len());
        #[rustfmt::skip]
        let want = [
            25.0, 3.0, 45.0, 1.0, 1.0,                  // amount, count, angle, enable, mode
            50.0 * 0.5 + 10.0, 75.0 * 0.5 + 20.0,       // center (0.25, 0.75) of 200x100
            1.0, 0.5, 0.25, 1.0,                        // tint
            100.0 * 0.5 + 10.0, 50.0 * 0.5 + 20.0, 7.0, // p3 (centre) + z
            1.0,                                        // p3_z (the slider)
            10.0 * 0.5 + 10.0, 20.0 * 0.5 + 20.0, -3.0, // fixed3
            -1.0,                                       // other: None
        ];
        assert_eq!(flat, want);

        // animated and edited values
        let t = Tick(1000);
        e.params.get_mut("amount").unwrap().keyframes =
            vec![Keyframe::new(Tick::ZERO, ParamValue::Float(0.0)), Keyframe::new(Tick(2000), ParamValue::Float(100.0))];
        e.params.get_mut("center").unwrap().value = ParamValue::Vec2(Vec2::new(4.0, 8.0));
        e.params.get_mut("other").unwrap().value = ParamValue::Choice(3);
        e.params.get_mut("p3_z2").unwrap().value = ParamValue::Float(-9.0);
        e.params.get_mut("enable").unwrap().value = ParamValue::Bool(false);
        e.params.get_mut("mode").unwrap().value = ParamValue::Choice(2);
        let flat = flatten(&m, &e, t, 2.0, [0.0, 0.0], layer);
        assert_eq!(flat.first(), Some(&50.0));
        assert_eq!(flat.get(3), Some(&0.0));
        assert_eq!(flat.get(4), Some(&2.0));
        assert_eq!(flat.get(5..7), Some(&[8.0, 16.0][..]));
        assert_eq!(flat.get(13), Some(&-9.0));
        assert_eq!(flat.last(), Some(&2.0), "Video 3 is layer index 2");

        let texts = flatten_texts(&m, &e);
        assert_eq!(texts, ["hello", "opaque"]);
        e.params.get_mut("blob").unwrap().value = ParamValue::Text("state".into());
        assert_eq!(flatten_texts(&m, &e), ["hello", "state"]);
    }

    #[test]
    fn flatten_survives_hostile_values() {
        let def = register("org.test.registry_hostile");
        let m = manifest("org.test.registry_hostile");
        let layer = [64.0, 32.0];
        // missing everything: defaults
        let empty = EffectInstance { params: Default::default(), ..def.instance() };
        let flat = flatten(&m, &empty, Tick::ZERO, 1.0, [0.0, 0.0], layer);
        assert_eq!(flat.len(), m.flat_len());
        assert_eq!(flat.get(5..7), Some(&[16.0, 24.0][..]), "missing point = default fraction x layer size");
        assert_eq!(flat.last(), Some(&-1.0));
        assert_eq!(flatten_texts(&m, &empty), ["hello", "opaque"]);
        // wrong types, out-of-range choices, non-finite values, unresolved (NaN) points
        let mut e = def.instance();
        e.params.get_mut("amount").unwrap().value = ParamValue::Text("x".into());
        e.params.get_mut("mode").unwrap().value = ParamValue::Choice(u32::MAX);
        e.params.get_mut("other").unwrap().value = ParamValue::Choice(u32::MAX);
        e.params.get_mut("tint").unwrap().value = ParamValue::Float(f64::NAN);
        e.params.get_mut("label").unwrap().value = ParamValue::Float(1.0);
        e.params.get_mut("angle").unwrap().value = ParamValue::Float(f64::INFINITY);
        let flat = flatten(&m, &e, Tick(i64::MIN), f64::NAN, [f64::INFINITY, 0.0], layer);
        assert_eq!(flat.len(), m.flat_len());
        assert_eq!(flat.first(), Some(&25.0));
        assert_eq!(flat.get(2), Some(&45.0));
        assert_eq!(flat.get(4), Some(&2.0), "popup clamped to the last option");
        assert_eq!(flat.get(7..11), Some(&[1.0, 0.5, 0.25, 1.0][..]));
        assert_eq!(flat.last(), Some(&7.0), "layer clamped to Video 8");
        assert_eq!(flatten_texts(&m, &e), ["hello", "opaque"]);
        let flat = flatten(&m, &e, Tick::ZERO, 1.0, [0.0, 0.0], layer);
        assert_eq!(flat.get(5..7), Some(&[16.0, 24.0][..]), "unresolved point = default fraction x layer size");
    }

    #[test]
    fn effect_defs_hold_only_built_ins() {
        register("org.test.registry_builtins");
        assert!(crate::effect_defs().iter().all(|d| !d.id.starts_with("org.test.")));
        let mut ids = std::collections::HashSet::new();
        assert!(crate::all_effect_defs().iter().all(|d| ids.insert(d.id)), "ids stay unique");
    }
}
