//! An OpenFX (OFX 1.4) image-effect host for FilmCraft.
//!
//! This crate loads native OFX plug-in bundles (RE:Vision ReelSmart Motion Blur, Boris FX
//! Sapphire, the free OFX collections, ...) and registers every plug-in as an ordinary FilmCraft
//! Video Effect through the native plug-in API v2 (`filmcraft_project::plugin`): it shows in the
//! Effects panel under Video Effects ▸ <the plug-in's grouping>, its parameters show in Effect
//! Controls and take keyframes, and it is applied to a clip by id like a built-in effect. It is a
//! CPU host: float premultiplied RGBA images (byte / short / RGB / alpha plug-ins are converted),
//! no GPU or OpenGL, no overlay interacts. See `docs/ofx.md` for usage and how it fits together.
//!
//! **This crate runs native code in-process, without a sandbox.** It is opt-in: only apps built
//! with their `openfx` feature link it. It needs `unsafe` to speak the C ABI, so (with
//! `crates/platform`) it is the only crate that does not opt into the workspace's
//! `unsafe_code = "forbid"` lint; every host callback is wrapped in `catch_unwind`, null-checks
//! its pointers and validates handles by magic number. On wasm32 the crate is empty.
//!
//! OFX's extra input clips (a `General` effect's matte, a transition's `SourceTo`) become Layer
//! parameters of the plug-in API, which FilmCraft shows as a choice of video track; the host
//! fetches them through [`PluginHost::layer_input`](filmcraft_project::plugin::PluginHost::layer_input).
//! Internally "layer" means the clip's frame the effect renders on (the API's term).
//!
//! Modules: [`ffi`] (the C ABI), [`props`] (property sets), [`params`] (parameter storage and the
//! mapping to plug-in API parameters), [`clips`] (clips, images and pixel conversion),
//! [`instance`] (effect handles, geometry, per-render context, actions), [`effect`] (the
//! `EffectPlugin` implementation), [`suites`] (the host suites), [`loader`] (bundle discovery
//! and registration) and [`blocklist`] (skips plug-ins that crashed the app while loading).

// Native code: nothing to host on the web build.
#![cfg(not(target_arch = "wasm32"))]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod blocklist;
pub mod clips;
pub mod effect;
pub mod ffi;
pub mod instance;
pub mod loader;
pub mod params;
pub mod props;
pub mod suites;

use std::path::PathBuf;

pub use blocklist::set_state_dir;
pub use loader::{default_search_paths, load_path};

/// One OFX plug-in that was registered as an effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedEffect {
    /// The FilmCraft effect id: the OFX identifier lower-cased (`ofx.` is prefixed if it
    /// would start with `fc.`, which FilmCraft reserves).
    pub id: String,
    /// Display name (the plug-in's label).
    pub name: String,
    /// Effects-panel folder under Video Effects: the first component of the plug-in's grouping.
    pub category: String,
    /// The OFX plug-in identifier.
    pub ofx_id: String,
    /// The OFX context the effect runs in: `filter`, `general`, `transition` or `generator`.
    pub context: String,
    /// The bundle folder (or binary) it came from.
    pub bundle: String,
}

/// The outcome of [`load_path`].
#[derive(Clone, Debug, Default)]
pub struct LoadReport {
    pub loaded: Vec<LoadedEffect>,
    pub errors: Vec<String>,
}

/// Engine hook: [`load_path`] as `{"loaded": [{id, name, category, ofxId, context, bundle}], "errors": [...]}`.
/// `Err` only when nothing could be loaded and there were errors.
pub fn loader(path: &str) -> Result<serde_json::Value, String> {
    let report = load_path(&PathBuf::from(path));
    if report.loaded.is_empty() && !report.errors.is_empty() {
        return Err(report.errors.join("; "));
    }
    let loaded: Vec<serde_json::Value> = report
        .loaded
        .iter()
        .map(|e| serde_json::json!({"id": e.id, "name": e.name, "category": e.category, "ofxId": e.ofx_id, "context": e.context, "bundle": e.bundle}))
        .collect();
    Ok(serde_json::json!({"loaded": loaded, "errors": report.errors}))
}

/// Helper programs plug-ins start and leave running after the host quits (Boris FX Sapphire
/// starts its preset browser on load).
const HELPERS: &[&str] = &["preset-browser.exe"];

/// Stops the plug-in helper programs ([`HELPERS`]) this process started, which would otherwise
/// keep running after it quits. Each leaked Sapphire `preset-browser.exe` holds about 340 MB, so a
/// few dozen runs of `filmcraft-cli` exhausted memory, and Sapphire then failed to describe its
/// effects ("Unknown error … Aborting OfxImageEffectActionDescribeInContext") and the process died
/// of a stack it could no longer commit. Only direct children with those names (and what they
/// started) are touched. Runs automatically when the process exits once plug-ins were loaded
/// ([`stop_helpers_at_exit`]); calling it earlier is harmless.
pub fn shutdown() {
    #[cfg(windows)]
    win::stop_children(HELPERS);
}

/// Make [`shutdown`] run when the process exits (returning from `main` or `std::process::exit`;
/// not when it is killed). Registered once.
pub(crate) fn stop_helpers_at_exit() {
    #[cfg(windows)]
    {
        extern "C" fn at_exit() {
            let _ = std::panic::catch_unwind(shutdown);
        }
        unsafe extern "C" {
            fn atexit(cb: extern "C" fn()) -> std::ffi::c_int;
        }
        static ONCE: std::sync::Once = std::sync::Once::new();
        // SAFETY: the C runtime's `atexit` with a callback that never unwinds.
        ONCE.call_once(|| unsafe {
            atexit(at_exit);
        });
    }
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    #[repr(C)]
    struct ProcessEntry32W {
        size: u32,
        usage: u32,
        pid: u32,
        default_heap_id: usize,
        module_id: u32,
        threads: u32,
        parent_pid: u32,
        pri_class_base: i32,
        flags: u32,
        exe_file: [u16; 260],
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut c_void;
        fn Process32FirstW(snap: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snap: *mut c_void, entry: *mut ProcessEntry32W) -> i32;
        fn CloseHandle(h: *mut c_void) -> i32;
        fn GetCurrentProcessId() -> u32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn TerminateProcess(h: *mut c_void, code: u32) -> i32;
    }
    const TH32CS_SNAPPROCESS: u32 = 2;
    const PROCESS_TERMINATE: u32 = 1;

    /// Terminate this process's direct children whose executable is one of `names`, together with
    /// everything they started (packaged helpers relaunch themselves as a child).
    pub fn stop_children(names: &[&str]) {
        let procs = snapshot();
        // SAFETY: no arguments, no failure mode.
        let me = unsafe { GetCurrentProcessId() };
        let mut doomed: Vec<u32> = procs.iter().filter(|p| p.1 == me && names.iter().any(|n| p.2.eq_ignore_ascii_case(n))).map(|p| p.0).collect();
        let mut i = 0;
        while let Some(&pid) = doomed.get(i) {
            for p in procs.iter().filter(|p| p.1 == pid && p.0 != pid) {
                if !doomed.contains(&p.0) && doomed.len() < 256 {
                    doomed.push(p.0);
                }
            }
            i += 1;
        }
        for pid in doomed {
            // SAFETY: a handle opened for termination only, and closed.
            unsafe {
                let h = OpenProcess(PROCESS_TERMINATE, 0, pid);
                if !h.is_null() {
                    TerminateProcess(h, 0);
                    CloseHandle(h);
                }
            }
        }
    }

    /// (pid, parent pid, executable name) of every process.
    pub fn snapshot() -> Vec<(u32, u32, String)> {
        let mut out = Vec::new();
        // SAFETY: plain Win32 calls on a process snapshot; the entry struct has the layout and
        // size the API requires, and the snapshot handle is closed.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap.is_null() || snap as isize == -1 {
                return out;
            }
            let mut e: ProcessEntry32W = std::mem::zeroed();
            e.size = std::mem::size_of::<ProcessEntry32W>() as u32;
            let mut more = Process32FirstW(snap, &mut e) != 0;
            while more && out.len() < 1 << 16 {
                let len = e.exe_file.iter().position(|&c| c == 0).unwrap_or(e.exe_file.len());
                out.push((e.pid, e.parent_pid, String::from_utf16_lossy(e.exe_file.get(..len).unwrap_or_default())));
                more = Process32NextW(snap, &mut e) != 0;
            }
            CloseHandle(snap);
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Regression: Sapphire's `preset-browser.exe` outlived every FilmCraft run (EffectCraft
        /// stops it at exit; the port had lost that). Starts a child under a helper's name and
        /// checks it is stopped, and that other children are left alone.
        #[test]
        fn helpers_this_process_started_are_stopped() {
            let dir = std::env::temp_dir().join(format!("fc-ofx-helpers-{}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            let ping = std::path::Path::new(&std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into())).join("System32").join("PING.EXE");
            let helper = dir.join("fc-test-helper.exe");
            std::fs::copy(&ping, &helper).unwrap();
            let mut h = std::process::Command::new(&helper).args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
            let mut other = std::process::Command::new(&ping).args(["-n", "30", "127.0.0.1"]).stdout(std::process::Stdio::null()).spawn().unwrap();
            assert!(snapshot().iter().any(|p| p.0 == h.id()));
            stop_children(&["FC-TEST-HELPER.EXE"]);
            // ping -n 30 runs for ~30 s on its own
            let t = std::time::Instant::now();
            while h.try_wait().unwrap().is_none() && t.elapsed() < std::time::Duration::from_secs(10) {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            assert!(h.try_wait().unwrap().is_some() && t.elapsed() < std::time::Duration::from_secs(10), "the helper is stopped");
            assert!(other.try_wait().unwrap().is_none(), "other children keep running");
            let _ = other.kill();
            let _ = other.wait();
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
