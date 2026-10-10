use std::sync::{Arc, Mutex};

use filmcraft_geom::Affine;
use filmcraft_project::plugin::{EffectPlugin, PluginFrame, PluginHost, PluginManifest, PluginParam, PluginParamKind, PluginParams, register_plugin};
use filmcraft_project::{EffectInstance, Keyframe, ParamValue, find_effect};
use filmcraft_time::Tick;

use super::*;
use crate::vfx::FxEnv;

fn cx() -> FxCtx<'static> {
    FxCtx {
        t: Tick::ZERO,
        px_scale: 1.0,
        seconds: 0.0,
        timecode: "",
        clip_name: "clip",
        project: None,
        env: None,
        working: filmcraft_color::WorkingSpace::Rec709,
    }
}

fn manifest(id: &str, params: Vec<PluginParam>) -> PluginManifest {
    PluginManifest {
        api: 2,
        id: id.into(),
        name: format!("Render Test {id}"),
        category: "Render Tests".into(),
        version: String::new(),
        author: String::new(),
        description: String::new(),
        params,
    }
}

fn param(id: &str, kind: PluginParamKind) -> PluginParam {
    PluginParam { id: id.into(), name: id.into(), group: String::new(), kind }
}

fn instance(id: &str) -> EffectInstance {
    find_effect(id).unwrap().instance()
}

/// A 4x3 image with a few colours (one transparent, one half transparent, one super-white).
fn test_image() -> Image {
    let mut img = Image::filled(4, 3, [0.2, 0.5, 0.8, 1.0]);
    img.px[0..4].copy_from_slice(&[0.0; 4]);
    img.px[4..8].copy_from_slice(&[0.1, 0.0, 0.3, 0.5]);
    img.px[8..12].copy_from_slice(&[2.0, 1.5, -0.1, 1.0]);
    img
}

fn close(a: &[f32], b: &[f32], tol: f32) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tol * x.abs().max(y.abs()).max(1.0))
}

/// Inverts the colour of every pixel in the encoding plug-ins see (sRGB, premultiplied).
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
}

/// Leaves the frame alone.
struct Identity(PluginManifest);
impl EffectPlugin for Identity {
    fn manifest(&self) -> &PluginManifest {
        &self.0
    }
    fn render(&self, _: &mut PluginFrame, _: &PluginParams, _: f64) -> Result<(), String> {
        Ok(())
    }
}

/// Scribbles on the frame, then fails (error or panic).
struct Fails(PluginManifest, bool);
impl EffectPlugin for Fails {
    fn manifest(&self) -> &PluginManifest {
        &self.0
    }
    fn padding(&self, _: &PluginParams, _: f64, _: f64, _: [f64; 2], _: u64) -> u32 {
        2
    }
    fn render(&self, frame: &mut PluginFrame, _: &PluginParams, _: f64) -> Result<(), String> {
        frame.pixels.iter_mut().for_each(|p| *p = [9.0, 9.0, 9.0, 1.0]);
        if self.1 {
            panic!("test plug-in panic");
        }
        Err("test plug-in error".into())
    }
}

/// Panics while measuring its padding.
struct PadPanics(PluginManifest);
impl EffectPlugin for PadPanics {
    fn manifest(&self) -> &PluginManifest {
        &self.0
    }
    fn padding(&self, _: &PluginParams, _: f64, _: f64, _: [f64; 2], _: u64) -> u32 {
        panic!("test padding panic");
    }
    fn render(&self, frame: &mut PluginFrame, _: &PluginParams, _: f64) -> Result<(), String> {
        frame.pixels.iter_mut().for_each(|p| *p = [9.0; 4]);
        Ok(())
    }
}

/// What [`Recorder`] saw in one render.
#[derive(Clone, Debug, Default)]
struct Seen {
    size: (u32, u32),
    origin: [f64; 2],
    scale: f64,
    layer_size: [f64; 2],
    time: f64,
    host_time: f64,
    key: u64,
    padding_key: u64,
    flat: Vec<f64>,
    corner: [f32; 4],
    first: [f32; 4],
    fps: f64,
    range: Option<(f64, f64)>,
    input: Option<(u32, u32, [f64; 2], [f32; 4])>,
    layer: Option<(u32, u32, [f64; 2], [f32; 4])>,
    params_later: Option<Vec<f64>>,
}

/// Records the geometry, time, parameters and host services of every render, then fills the
/// whole buffer (padding included) with opaque [`FILL`].
struct Recorder {
    m: PluginManifest,
    pad: u32,
    seen: Mutex<Vec<Seen>>,
    padding_key: Mutex<u64>,
}
const FILL: [f32; 4] = [0.5, 0.25, 0.0, 1.0];
impl EffectPlugin for Recorder {
    fn manifest(&self) -> &PluginManifest {
        &self.m
    }
    fn padding(&self, _: &PluginParams, _: f64, _: f64, _: [f64; 2], key: u64) -> u32 {
        *self.padding_key.lock().unwrap() = key;
        self.pad
    }
    fn render(&self, _: &mut PluginFrame, _: &PluginParams, _: f64) -> Result<(), String> {
        Err("render_v2 only".into())
    }
    fn render_v2(&self, frame: &mut PluginFrame, params: &PluginParams, time: f64, host: &dyn PluginHost) -> Result<(), String> {
        let at = |x: usize, y: usize| frame.pixels[y * frame.width as usize + x];
        let p = self.pad as usize;
        let first_px = |i: &PluginImage| (i.width, i.height, i.origin, i.pixels[(i.height as usize / 2) * i.width as usize + i.width as usize / 2]);
        let seen = Seen {
            size: (frame.width, frame.height),
            origin: frame.origin,
            scale: frame.scale,
            layer_size: frame.layer_size,
            time,
            host_time: host.time(),
            key: host.instance_key(),
            padding_key: *self.padding_key.lock().unwrap(),
            flat: params.flat.to_vec(),
            corner: at(0, 0),
            first: at(p + 1, p),
            fps: host.frame_rate(),
            range: host.frame_range(),
            input: host.input_at(time + 0.5).map(|i| first_px(&i)),
            layer: host.layer_input("other", time).map(|i| first_px(&i)),
            params_later: host.params_at(time + 0.5).map(|(f, _)| f),
        };
        self.seen.lock().unwrap().push(seen);
        frame.pixels.iter_mut().for_each(|p| *p = FILL);
        Ok(())
    }
}

fn recorder(id: &str, pad: u32) -> Arc<Recorder> {
    let m = manifest(
        id,
        vec![
            param("amount", PluginParamKind::Slider { default: 10.0, min: 0.0, max: 100.0, slider_min: None, slider_max: None, decimals: 2, integer: false }),
            param("center", PluginParamKind::Point { default: [0.25, 0.5] }),
            param("other", PluginParamKind::Layer),
        ],
    );
    let r = Arc::new(Recorder { m, pad, seen: Mutex::new(vec![]), padding_key: Mutex::new(0) });
    register_plugin(r.clone()).unwrap();
    r
}

/// A clip 2 s long, 0.5 s in, 4x3 source pixels; frames are a flat colour that encodes `dt`;
/// track `1` is flat green.
struct TestEnv {
    effects: Vec<EffectInstance>,
}
impl FxEnv for TestEnv {
    fn frame(&self, dt: f64, _upto: &EffectInstance) -> Option<Image> {
        Some(Image::filled(4, 3, [dt as f32, 0.0, 0.0, 1.0]))
    }
    fn source_frame(&self, _dt: f64, _scale: f32) -> Option<Image> {
        None
    }
    fn track(&self, index: usize) -> Option<Image> {
        (index == 1).then(|| Image::filled(4, 3, [0.0, 1.0, 0.0, 1.0]))
    }
    fn layer_to_output(&self) -> Affine {
        Affine::IDENTITY
    }
    fn clip_seconds(&self) -> f64 {
        2.0
    }
    fn clip_offset(&self) -> f64 {
        0.5
    }
    fn frame_rate(&self) -> f64 {
        24.0
    }
    fn media_timecode(&self) -> String {
        String::new()
    }
    fn file_name(&self) -> String {
        String::new()
    }
    fn sequence_name(&self) -> String {
        String::new()
    }
    fn sequence_size(&self) -> (u32, u32) {
        (4, 3)
    }
    fn source_size(&self) -> (u32, u32) {
        (4, 3)
    }
    fn clip_key(&self) -> u64 {
        1234
    }
    fn effect_ordinal(&self, e: &EffectInstance) -> Option<usize> {
        let mut n = 0;
        for x in &self.effects {
            if std::ptr::eq(x, e) {
                return Some(n);
            }
            n += usize::from(x.effect == e.effect);
        }
        None
    }
}

#[test]
fn a_plugin_effect_renders_in_srgb() {
    register_plugin(Arc::new(Invert(manifest("org.fctest.render.invert", vec![])))).unwrap();
    let e = instance("org.fctest.render.invert");
    let mut img = Image::filled(3, 2, [0.2, 0.5, 1.0, 1.0]);
    crate::effects::apply(&mut img, &e, &cx());
    let inv = |v: f32| filmcraft_color::srgb_to_linear(1.0 - filmcraft_color::linear_to_srgb(v));
    let want = [inv(0.2), inv(0.5), inv(1.0), 1.0];
    for p in img.px.as_chunks::<4>().0 {
        assert!(close(p, &want, 1e-5), "{p:?} != {want:?}");
    }
    // disabled: untouched
    let mut off = e.clone();
    off.enabled = false;
    let mut img = Image::filled(3, 2, [0.2, 0.5, 1.0, 1.0]);
    crate::effects::apply(&mut img, &off, &cx());
    assert_eq!(img, Image::filled(3, 2, [0.2, 0.5, 1.0, 1.0]));
}

#[test]
fn srgb_round_trip_keeps_the_image() {
    register_plugin(Arc::new(Identity(manifest("org.fctest.render.identity", vec![])))).unwrap();
    let e = instance("org.fctest.render.identity");
    let before = test_image();
    let mut img = before.clone();
    crate::effects::apply(&mut img, &e, &cx());
    assert!(close(&img.px, &before.px, 1e-5), "{:?} != {:?}", img.px, before.px);
    // the conversion itself: super-whites and negatives keep their sign and size
    let p = to_plugin([2.0, -0.5, 0.25, 0.5]);
    assert!(p[0] > 0.5 && p[1] < 0.0 && p[3] == 0.5);
    assert!(close(&from_plugin(p), &[2.0, -0.5, 0.25, 0.5], 1e-5));
    assert_eq!(to_plugin([1.0, 1.0, 1.0, 0.0]), [0.0; 4], "alpha 0 has no colour");
    assert_eq!(to_plugin([f32::NAN, f32::INFINITY, 1.0, 1.0])[..2], [0.0, 0.0]);
    assert_eq!(from_plugin([1.0, 1.0, 1.0, f32::NAN]), [0.0; 4]);
}

#[test]
fn padding_origin_points_and_crop() {
    let rec = recorder("org.fctest.render.recorder", 3);
    let e = instance("org.fctest.render.recorder");
    let mut img = test_image();
    let cx = FxCtx { px_scale: 1.0, seconds: 0.25, ..cx() };
    crate::effects::apply(&mut img, &e, &cx);
    let seen = rec.seen.lock().unwrap().clone();
    let s = seen.last().unwrap();
    assert_eq!(s.size, (4 + 6, 3 + 6), "3 transparent pixels on every side");
    assert_eq!(s.origin, [3.0, 3.0]);
    assert_eq!(s.scale, 1.0);
    assert_eq!(s.layer_size, [4.0, 3.0], "no environment: the image size / px_scale");
    assert_eq!(s.time, 0.25, "clip time (FxCtx::seconds)");
    assert_eq!(s.host_time, 0.25);
    assert_eq!(s.corner, [0.0; 4], "padding is transparent");
    let px1 = test_image().px[4..8].to_vec();
    assert!(close(&s.first, &to_plugin([px1[0], px1[1], px1[2], px1[3]]), 1e-6), "layer pixel (1, 0) at buffer (p+1, p)");
    // the point is measured from the padded origin: (0.25, 0.5) of 4x3 + 3
    assert_eq!(s.flat, [10.0, 1.0 + 3.0, 1.5 + 3.0, -1.0]);
    // without an environment nothing is available
    assert_eq!((s.key, s.fps, s.range), (0, 30.0, None));
    assert!(s.input.is_none() && s.layer.is_none() && s.params_later.is_none());
    // the whole frame came back (cropped), decoded to linear
    let fill = from_plugin(FILL);
    for p in img.px.as_chunks::<4>().0 {
        assert!(close(p, &fill, 1e-6));
    }

    // a scaled working image: the point follows the scale
    let mut half = Image::filled(2, 2, [0.1, 0.1, 0.1, 1.0]);
    crate::effects::apply(&mut half, &e, &FxCtx { px_scale: 0.5, ..cx });
    let s = rec.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!((s.size, s.scale, s.layer_size), ((8, 8), 0.5, [4.0, 4.0]));
    assert_eq!(s.flat.get(1..3), Some(&[1.0 * 0.5 + 3.0, 2.0 * 0.5 + 3.0][..]));
}

#[test]
fn the_host_serves_other_times_layers_and_parameters() {
    let rec = recorder("org.fctest.render.hosted", 1);
    let mut a = instance("org.fctest.render.hosted");
    a.params.get_mut("other").unwrap().value = ParamValue::Choice(2); // Video 2 = track index 1
    a.params.get_mut("amount").unwrap().keyframes =
        vec![Keyframe::new(Tick::ZERO, ParamValue::Float(0.0)), Keyframe::new(Tick::from_seconds_f64(1.0), ParamValue::Float(100.0))];
    let b = instance("org.fctest.render.hosted");
    let env = TestEnv { effects: vec![a, b] };
    let cx = FxCtx { seconds: 0.5, env: Some(&env), ..cx() };
    let mut img = test_image();
    crate::effects::apply(&mut img, &env.effects[0], &cx);
    let s = rec.seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(s.time, 0.5);
    assert_eq!(s.fps, 24.0);
    assert_eq!(s.range, Some((0.0, 2.0)));
    assert_ne!(s.key, 0);
    assert_eq!(s.key, s.padding_key, "padding and render see the same instance key");
    // the input 0.5 s later: env.frame(dt = 0.5), unpadded, origin 0
    let (w, h, origin, px) = s.input.unwrap();
    assert_eq!((w, h, origin), (4, 3, [0.0, 0.0]));
    assert!(close(&px, &to_plugin([0.5, 0.0, 0.0, 1.0]), 1e-6));
    // the other track, mapped into the padded buffer
    let (w, h, origin, px) = s.layer.unwrap();
    assert_eq!((w, h, origin), (6, 5, [1.0, 1.0]));
    assert!(close(&px, &to_plugin([0.0, 1.0, 0.0, 1.0]), 1e-6), "{px:?}");
    // parameters 0.5 s later on the keyframe clock: amount 50; points with the padded origin
    assert_eq!(s.flat.first(), Some(&0.0));
    assert_eq!(s.params_later.unwrap(), [50.0, 2.0, 2.5, 1.0]);

    // instance keys: stable for one applied effect, different for the second one
    let mut img = test_image();
    crate::effects::apply(&mut img, &env.effects[0], &cx);
    let again = rec.seen.lock().unwrap().last().unwrap().key;
    crate::effects::apply(&mut img, &env.effects[1], &cx);
    let second = rec.seen.lock().unwrap().last().unwrap().key;
    assert_eq!(again, s.key);
    assert!(second != 0 && second != s.key);
    // a clip without a layer chosen gets none, and other times have no layer
    let s2 = rec.seen.lock().unwrap().last().cloned().unwrap();
    assert!(s2.layer.is_none());
}

#[test]
fn a_failing_plugin_leaves_the_image_unchanged() {
    register_plugin(Arc::new(Fails(manifest("org.fctest.render.error", vec![]), false))).unwrap();
    register_plugin(Arc::new(Fails(manifest("org.fctest.render.panic", vec![]), true))).unwrap();
    register_plugin(Arc::new(PadPanics(manifest("org.fctest.render.padpanic", vec![])))).unwrap();
    for id in ["org.fctest.render.error", "org.fctest.render.panic", "org.fctest.render.padpanic"] {
        let e = instance(id);
        for _ in 0..2 {
            let mut img = test_image();
            crate::effects::apply(&mut img, &e, &cx());
            assert_eq!(img, test_image(), "{id}");
        }
    }
}

#[test]
fn plugins_never_take_the_gpu_path() {
    register_plugin(Arc::new(Identity(manifest("org.fctest.render.gpu", vec![])))).unwrap();
    let e = instance("org.fctest.render.gpu");
    assert!(crate::gpufx::FxOp::eval(&e, &cx(), 4, 4).is_none());
    assert!(!crate::gpufx::GPU_EFFECTS.contains(&e.effect.as_str()));
    // unknown ids are not plug-ins
    let mut img = test_image();
    assert!(!apply(&mut img, &EffectInstance { effect: "org.fctest.render.missing".into(), ..e }, &cx()));
}

#[test]
fn padding_fits_the_pixel_budget() {
    assert_eq!(fit_padding(100, 100, 50), 50);
    let p = fit_padding(8000, 8000, 4096);
    assert!(p < 4096);
    assert!((8000 + 2 * p as usize) * (8000 + 2 * p as usize) <= MAX_PADDED_PIXELS);
    assert!((8000 + 2 * (p as usize + 1)) * (8000 + 2 * (p as usize + 1)) > MAX_PADDED_PIXELS);
    assert_eq!(fit_padding(20_000, 20_000, 10), 0, "already over the budget: no padding");
    assert_eq!(fit_padding(usize::MAX, 2, u32::MAX), 0);
}
