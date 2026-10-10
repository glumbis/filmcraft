//! Plug-in effects ([`filmcraft_project::plugin`]): runs a registered [`EffectPlugin`] (an OpenFX
//! effect, for example) on a clip's working image.
//!
//! The working image is premultiplied **linear** f32; plug-ins get premultiplied **sRGB-encoded**
//! pixels (the plug-in contract), converted on the way in and out. The transfer is
//! sign-preserving and extended above 1, so super-whites and negative values survive a round
//! trip.
//!
//! * Geometry: `scale` is [`FxCtx::px_scale`] (working pixels per clip source pixel) and the
//!   layer is the clip's source frame ([`crate::vfx::FxEnv::source_size`]).
//! * Time: `time` is **clip time**, seconds since the clip's first frame on the timeline
//!   ([`FxCtx::seconds`]). Other times a plug-in asks for are on the same clock: the input at
//!   `t2` is the clip's frame `t2 - time` timeline seconds from now, parameters at `t2` are read
//!   on the keyframe clock at that timeline time, and the frame range is `(0, clip duration)`
//!   (like effects in Premiere, a plug-in sees only the part of the clip that is on the
//!   timeline).
//! * Padding: the plug-in may ask for transparent pixels around the clip
//!   ([`EffectPlugin::padding`], at most [`MAX_PADDING`] and [`MAX_PADDED_PIXELS`] in all) so
//!   blurs and glows have room; the result is cropped back to the clip's frame (effects stop at
//!   the clip's edges, as in Premiere).
//! * Failure: an error or a panic in the plug-in leaves the image unchanged and is logged once
//!   per effect id.

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Mutex, OnceLock, PoisonError};

use filmcraft_geom::Affine;
use filmcraft_project::plugin::{self as reg, EffectPlugin, MAX_PADDING, PluginFrame, PluginHost, PluginImage, PluginManifest, PluginParamKind, PluginParams};
use filmcraft_project::{EffectInstance, ParamValue};
use filmcraft_time::Tick;
use rayon::prelude::*;

use crate::effects::FxCtx;
use crate::image::Image;

/// The most pixels a padded plug-in buffer may have (the padding is reduced to fit).
pub const MAX_PADDED_PIXELS: usize = 64 << 20;

/// Times a plug-in asks for are clamped to this many seconds around now (hostile values must not
/// overflow tick arithmetic).
const MAX_DT: f64 = 1.0e5;

/// Run `e` if it is a registered plug-in effect. False when it is not (the caller renders it).
pub(crate) fn apply(img: &mut Image, e: &EffectInstance, cx: &FxCtx) -> bool {
    let Some(plugin) = reg::plugin(&e.effect) else { return false };
    run(img, e, cx, plugin.as_ref());
    true
}

/// Log a plug-in failure once per effect id.
fn warn_once(id: &str, msg: &str) {
    static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let mut seen = WARNED.get_or_init(Default::default).lock().unwrap_or_else(PoisonError::into_inner);
    if seen.len() < 4096 && seen.insert(id.to_string()) {
        log::warn!("plug-in effect `{id}`: {msg} (the effect is skipped)");
    }
}

/// A panic payload as text.
fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panicked".into())
}

/// The stable key of one applied plug-in effect on one clip: the clip's identity, the effect id
/// and which of the clip's effects with that id it is. Never 0 (0 = unknown, no environment).
pub(crate) fn instance_key(e: &EffectInstance, cx: &FxCtx) -> u64 {
    let Some(env) = cx.env else { return 0 };
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut mix = |b: &[u8]| {
        for &x in b {
            h = (h ^ x as u64).wrapping_mul(0x0100_0000_01b3);
        }
    };
    mix(&env.clip_key().to_le_bytes());
    mix(e.effect.as_bytes());
    mix(&[0xff]);
    if let Some(n) = env.effect_ordinal(e) {
        mix(&(n as u64).to_le_bytes());
    }
    h.max(1)
}

/// The largest padding `<= pad` that keeps a `w x h` image padded on every side within
/// [`MAX_PADDED_PIXELS`].
pub(crate) fn fit_padding(w: usize, h: usize, pad: u32) -> u32 {
    let fits = |p: u32| {
        let p2 = (p as usize).saturating_mul(2);
        w.saturating_add(p2).checked_mul(h.saturating_add(p2)).is_some_and(|n| n <= MAX_PADDED_PIXELS)
    };
    if fits(pad) {
        return pad;
    }
    // (w + 2p)(h + 2p) <= N  ⇔  4p² + 2(w + h)p + wh - N <= 0
    let (wf, hf, n) = (w as f64, h as f64, MAX_PADDED_PIXELS as f64);
    let disc = (wf + hf).powi(2) - 4.0 * (wf * hf - n);
    let mut p = if disc >= 0.0 { ((-(wf + hf) + disc.sqrt()) / 4.0).floor().clamp(0.0, pad as f64) as u32 } else { 0 };
    while p > 0 && !fits(p) {
        p -= 1;
    }
    p
}

// ---------------------------------------------------------------- colour

/// Linear → sRGB, sign-preserving and extended above 1; non-finite values become 0.
#[inline]
fn encode(v: f32) -> f32 {
    if !v.is_finite() {
        return 0.0;
    }
    let s = filmcraft_color::linear_to_srgb(v.abs());
    if v < 0.0 { -s } else { s }
}

/// sRGB → linear, the inverse of [`encode`].
#[inline]
fn decode(v: f32) -> f32 {
    if !v.is_finite() {
        return 0.0;
    }
    let l = filmcraft_color::srgb_to_linear(v.abs());
    if v < 0.0 { -l } else { l }
}

/// Premultiplied linear → premultiplied sRGB-encoded (transparent pixels are all zero).
#[inline]
pub(crate) fn to_plugin(p: [f32; 4]) -> [f32; 4] {
    let a = p[3];
    if !(a.is_finite() && a > 0.0) {
        return [0.0; 4];
    }
    [encode(p[0] / a) * a, encode(p[1] / a) * a, encode(p[2] / a) * a, a]
}

/// Premultiplied sRGB-encoded → premultiplied linear (transparent pixels are all zero).
#[inline]
pub(crate) fn from_plugin(p: [f32; 4]) -> [f32; 4] {
    let a = p[3];
    if !(a.is_finite() && a > 0.0) {
        return [0.0; 4];
    }
    [decode(p[0] / a) * a, decode(p[1] / a) * a, decode(p[2] / a) * a, a]
}

/// A working image as plug-in pixels.
fn image_pixels(img: &Image) -> Vec<[f32; 4]> {
    img.px.par_chunks_exact(4).map(|p| to_plugin([p[0], p[1], p[2], p[3]])).collect()
}

fn plugin_image(img: &Image, origin: [f64; 2], scale: f64) -> Option<PluginImage> {
    if img.w == 0 || img.h == 0 || img.px.len() != img.w.checked_mul(img.h)?.checked_mul(4)? {
        return None;
    }
    Some(PluginImage { width: u32::try_from(img.w).ok()?, height: u32::try_from(img.h).ok()?, pixels: image_pixels(img), origin, scale })
}

// ---------------------------------------------------------------- render

/// Everything one render of a plug-in effect needs to know about its frame.
struct Geometry {
    scale: f64,
    layer_size: [f64; 2],
    origin: [f64; 2],
    /// Buffer size.
    w: usize,
    h: usize,
    time: f64,
    key: u64,
}

fn run(img: &mut Image, e: &EffectInstance, cx: &FxCtx, plugin: &dyn EffectPlugin) {
    let m = plugin.manifest();
    let (w, h) = (img.w, img.h);
    if w == 0 || h == 0 || img.px.len() != w.saturating_mul(h).saturating_mul(4) {
        return;
    }
    let scale = Some(cx.px_scale as f64).filter(|s| s.is_finite() && *s > 0.0).unwrap_or(1.0);
    let layer_size = match cx.env.map(|env| env.source_size()) {
        Some((sw, sh)) if sw > 0 && sh > 0 => [sw as f64, sh as f64],
        _ => [w as f64 / scale, h as f64 / scale],
    };
    let time = if cx.seconds.is_finite() { cx.seconds } else { 0.0 };
    let key = instance_key(e, cx);
    let texts = reg::flatten_texts(m, e);
    let flat = reg::flatten(m, e, cx.t, scale, [0.0, 0.0], layer_size);
    let asked = catch_unwind(AssertUnwindSafe(|| plugin.padding(&PluginParams { manifest: m, flat: &flat, texts: &texts }, time, scale, layer_size, key)));
    let pad = match asked {
        Ok(p) => fit_padding(w, h, p.min(MAX_PADDING)),
        Err(p) => {
            warn_once(&m.id, &format!("panicked while measuring its padding: {}", panic_text(p.as_ref())));
            return;
        }
    };
    let p = pad as usize;
    let (bw, bh) = (w + 2 * p, h + 2 * p);
    let (Ok(fw), Ok(fh)) = (u32::try_from(bw), u32::try_from(bh)) else { return };
    // the working image, sRGB premultiplied, in the middle of a transparent buffer
    let mut buf = vec![[0.0f32; 4]; bw * bh];
    buf.par_chunks_mut(bw).skip(p).take(h).enumerate().for_each(|(y, row)| {
        let src = img.px.get(y * w * 4..(y + 1) * w * 4).unwrap_or(&[]);
        for (d, s) in row.iter_mut().skip(p).zip(src.as_chunks::<4>().0) {
            *d = to_plugin(*s);
        }
    });
    let origin = [p as f64, p as f64];
    // padding moves the layer origin, which points are measured from
    let flat = if pad > 0 { reg::flatten(m, e, cx.t, scale, origin, layer_size) } else { flat };
    let g = Geometry { scale, layer_size, origin, w: bw, h: bh, time, key };
    let host = Host { cx, e, m, g: &g };
    let params = PluginParams { manifest: m, flat: &flat, texts: &texts };
    let result = catch_unwind(AssertUnwindSafe(|| {
        let mut frame = PluginFrame { width: fw, height: fh, pixels: &mut buf, scale, origin, layer_size };
        plugin.render_v2(&mut frame, &params, time, &host)
    }));
    match result {
        Ok(Ok(())) => {}
        Ok(Err(msg)) => return warn_once(&m.id, &msg),
        Err(p) => return warn_once(&m.id, &format!("panicked: {}", panic_text(p.as_ref()))),
    }
    if buf.len() != bw * bh {
        return warn_once(&m.id, "changed the size of its buffer");
    }
    // crop the clip's frame back out
    img.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        let Some(src) = buf.get((y + p) * bw + p..(y + p) * bw + p + w) else { return };
        for (d, s) in row.as_chunks_mut::<4>().0.iter_mut().zip(src) {
            *d = from_plugin(*s);
        }
    });
}

/// The host services of one render ([`PluginHost`]) over the clip's [`crate::vfx::FxEnv`].
/// Without an environment (isolated renders, adjustment layers, frames read for temporal
/// effects) every service is unavailable.
struct Host<'a, 'c> {
    cx: &'a FxCtx<'c>,
    e: &'a EffectInstance,
    m: &'a PluginManifest,
    g: &'a Geometry,
}

impl Host<'_, '_> {
    /// Timeline seconds from now of clip time `t2`, if it is a usable time.
    fn dt(&self, t2: f64) -> Option<f64> {
        let dt = t2 - self.g.time;
        dt.is_finite().then_some(dt.clamp(-MAX_DT, MAX_DT))
    }
    fn fps(&self) -> f64 {
        self.cx.env.map(|env| env.frame_rate()).filter(|f| f.is_finite() && *f > 0.0).unwrap_or(30.0)
    }
}

impl PluginHost for Host<'_, '_> {
    fn input_at(&self, layer_time: f64) -> Option<PluginImage> {
        let env = self.cx.env?;
        let dt = self.dt(layer_time)?;
        let img = env.frame(dt, self.e)?;
        plugin_image(&img, [0.0, 0.0], self.g.scale)
    }

    fn layer_input(&self, param_id: &str, layer_time: f64) -> Option<PluginImage> {
        let env = self.cx.env?;
        // other tracks are rendered at the current time only
        let near = (layer_time - self.g.time).abs() * self.fps() < 1.0e-3;
        if !near {
            return None;
        }
        self.m.params.iter().find(|p| p.id == param_id && matches!(p.kind, PluginParamKind::Layer))?;
        let choice = match self.e.param(param_id).map(|p| &p.value) {
            Some(ParamValue::Choice(c)) => *c as usize,
            _ => 0,
        };
        let track = env.track(choice.checked_sub(1)?)?;
        // output pixels → layer pixels → this buffer (shifted by the padding)
        let to_buffer = Affine::translate(self.g.origin[0], self.g.origin[1]).then_apply(&env.layer_to_output().inverse()?);
        let img = track.transformed(self.g.w, self.g.h, &to_buffer);
        plugin_image(&img, self.g.origin, self.g.scale)
    }

    fn params_at(&self, layer_time: f64) -> Option<(Vec<f64>, Vec<String>)> {
        let dt = self.dt(layer_time)?;
        let env = self.cx.env?;
        let t = env.effect_time(dt).unwrap_or_else(|| Tick(self.cx.t.0.saturating_add(Tick::from_seconds_f64(dt).0)));
        Some((reg::flatten(self.m, self.e, t, self.g.scale, self.g.origin, self.g.layer_size), reg::flatten_texts(self.m, self.e)))
    }

    fn frame_rate(&self) -> f64 {
        self.fps()
    }

    fn time(&self) -> f64 {
        self.g.time
    }

    fn instance_key(&self) -> u64 {
        self.g.key
    }

    fn frame_range(&self) -> Option<(f64, f64)> {
        let secs = self.cx.env?.clip_seconds();
        (secs.is_finite() && secs > 0.0).then_some((0.0, secs))
    }
}

#[cfg(test)]
#[path = "plugin_fx_tests.rs"]
mod tests;
