# OpenFX plug-ins

FilmCraft can load [OpenFX](https://openfx.readthedocs.io/) (OFX) image-effect plug-ins and use
them as ordinary video effects: commercial suites that ship as OFX (for example RE:Vision ReelSmart
Motion Blur and Twixtor, Boris FX Sapphire) and the free OFX collections run alongside the built-in
effects. OFX is the open plug-in standard of Nuke, DaVinci Resolve, Vegas, Natron and others; its
API headers are BSD-3-Clause.

An OFX effect behaves like any other FilmCraft video effect: it appears in the Effects panel under
**Video Effects ▸ <the plug-in's own group>** (for example *Sapphire Stylize*, *REVision Effects*),
you drag it onto a clip or double-click it, its settings show up in **Effect Controls**, and they
take keyframes. The effect id is the OFX identifier, lower-cased (for example
`org.filmcraft.test.invert`).

## Opt-in: the `openfx` feature

OFX plug-ins are native libraries that run inside the app's process, **without a sandbox**. Hosting
them also needs `unsafe` Rust to call the C interface, which the rest of the workspace forbids
([AGENTS.md](../AGENTS.md) §0.3). So the host is a separate crate (`crates/ofx`, `filmcraft-ofx`),
the only one besides `crates/platform` allowed `unsafe`, and it is **off by default**: builds only
include it with the `openfx` feature on the `filmcraft` app and `filmcraft-cli`.

```sh
cargo run --release -p filmcraft --features openfx           # the desktop app
cargo build --release -p filmcraft -p filmcraft-cli --features openfx
```

Without the feature the OFX commands are disabled ("OpenFX plug-ins are not available in this
build") and nothing native is loaded. There is no OFX on the web build.

## Installing OFX plug-ins

Install the plug-in with its own installer. OFX plug-ins live in `.ofx.bundle` folders in standard
locations, and FilmCraft looks in the same places as other hosts:

| OS | Standard folder |
|---|---|
| Windows | `C:\Program Files\Common Files\OFX\Plugins` |
| macOS | `/Library/OFX/Plugins` |
| Linux | `/usr/OFX/Plugins` |

In addition, every folder listed in the `OFX_PLUGIN_PATH` environment variable is searched
(separated by `;` on Windows, `:` elsewhere). Folders are scanned recursively for bundles.

Nothing needs configuring: at start-up the app scans these folders (the `effects.plugins.scanOfx`
command) and registers everything it finds. Large suites register many effects (Sapphire around
290); loading them takes a few seconds.

## Loading a plug-in by hand

* **File ▸ Load OpenFX Plug-in…** asks for an `.ofx` binary (inside
  `X.ofx.bundle/Contents/<platform>`).
* Commands (control channel, CLI, MCP): `effects.plugins.loadOfx {"path": "..."}` loads an `.ofx`
  binary, an `.ofx.bundle` folder or a folder to scan, and returns the loaded effects and any
  errors; `effects.plugins.scanOfx {}` rescans every default location and `OFX_PLUGIN_PATH` and
  returns `{paths, loaded, errors}`; `effects.plugins.list` lists the registered plug-ins.
* To skip the start-up scan (for example to rule OFX out when something misbehaves) set the
  environment variable `FILMCRAFT_NO_OFX=1`.

## A plug-in that crashes while loading

A native crash inside a plug-in (in its library start-up code, `Load`, `Describe` or
`DescribeInContext`) takes the whole app down, and since the standard folders are scanned at every
start-up, one bad plug-in would make FilmCraft crash on every launch. So, like other OFX hosts,
FilmCraft keeps a **crash blocklist** in its data folder (`%APPDATA%\FilmCraft` on Windows,
`~/Library/Application Support/FilmCraft` on macOS, `$XDG_DATA_HOME/filmcraft` or
`~/.local/share/filmcraft` on Linux, or `FILMCRAFT_DATA_DIR`):

* Before it opens a plug-in binary the process writes the binary's path to `ofx_loading.<pid>.txt`,
  keeps that file locked, and deletes it once the binary has been loaded and described
  (successfully or with an ordinary error).
* If the process dies in between, the file is still there and its lock is gone. A process that was
  killed (the app closed while it was starting, a script's timeout) leaves the same file as one
  that crashed, and big suites take a while to load (Sapphire about two seconds), so one death only
  makes the binary a suspect: the next process that loads plug-ins records it in
  `ofx_suspects.txt` and still loads it, and a successful load clears it. If a load dies in the
  same binary again first, its path moves to `ofx_blocklist.txt` (one path per line) and the
  binary is **skipped** from then on: a plug-in that crashes while loading takes FilmCraft down
  twice, not on every launch. The skip shows up in the loader's `errors` ("... skipped: it crashed
  FilmCraft while loading last time ...") and on stderr at start-up. The rest of the plug-ins load
  normally. A marker that is still locked belongs to another FilmCraft process (the CLI next to the
  app) that is loading right now, and is left alone.
* To try a blocklisted plug-in again (after updating or re-licensing it), run
  `effects.plugins.scanOfx {"retry": true}`, or delete its line (or the whole `ofx_blocklist.txt`)
  and restart. A plug-in that crashes again is simply blocklisted again.

This only covers loading. A crash while an effect is rendering is not caught and is not
blocklisted; if that keeps happening, start with `FILMCRAFT_NO_OFX=1` or uninstall the plug-in.
Every host callback into Rust runs under `catch_unwind`, so a panic in the host never unwinds into
the plug-in's C code.

## How OFX maps onto FilmCraft

* **Rendering is CPU.** The plug-in gets the clip's working image as premultiplied float RGBA,
  sRGB-encoded. Plug-ins that only support 8- or 16-bit are converted.
* **Confined to the clip's frame**, like effects in Premiere: the plug-in sees transparent padding
  around the clip (so blurs and glows have room to spread) but whatever it draws outside the clip's
  frame is cropped.
* Contexts: Filter is preferred, then General, Transition and Generator. A General effect's extra
  input clips (every clip except `Source`) become **Layer** parameters named after the clip: pick
  the video track to use as that input. A Transition's `SourceTo` clip works the same way.
* Parameters: doubles and integers become sliders, booleans checkboxes, choices popups, RGBA
  colours, strings text fields; custom parameters carry opaque plug-in data and are hidden. Groups
  and pages are folded into parameter names. They are keyframable in Effect Controls.
* 2D and 3D doubles follow their OFX double type. Only position values (type XY or XY absolute)
  become an on-screen point; every other 2D or 3D double becomes one slider per component, named
  `<name>_x`, `_y` and `_z`.
* Temporal plug-ins (motion blur, echo, retiming) can ask for the input at other times; FilmCraft
  reads those other frames of the clip on demand. Each applied effect gets its own pool of OFX
  instances, so plug-ins that keep state between frames never mix two clips up.

## Retimers (Twixtor)

Time-remapping plug-ins such as RE:Vision **Twixtor** (OFX version) read other frames of the clip
and need the clip's real source range; see [FORK.md](../FORK.md) for how each tested plug-in
behaves. In General context their optional extra inputs (motion vectors, foreground matte) are
Layer parameters like any other extra clip. `paramGetIntegral` and `paramGetDerivative` sample the
animated value through the host so a plug-in can integrate a keyframed Speed % curve.

## Licensing

FilmCraft does not include, license or activate any third-party plug-in.

* Commercial plug-ins need their own OFX licences. Without one they render a watermark or report an
  "unlicensed" status; vendors often sell OFX licences separately from their Premiere ones.
  Licence activation is done with the vendor's own tools.
* The host identifies itself to plug-ins as "FilmCraft". It is **not a supported host** for any
  vendor: do not ask the vendor for support, and a plug-in whose licence is tied to a list of known
  hosts may refuse to run.

## Known limitations

* **No GPU.** Plug-ins are rendered on the CPU through the OFX CPU API. Plug-ins that only render
  with OpenGL, CUDA or Metal fall back to their CPU path, or fail to load if they have none.
* **No on-screen overlays.** The plug-ins' viewer widgets (handles, tracking boxes) do not appear;
  use the parameters instead. Push buttons that open dialogs do nothing.
* **Native code runs in-process.** An OFX plug-in is a native library with full access to the
  machine. A bug in one can crash FilmCraft, and a malicious one can do anything the app can. Only
  install plug-ins you trust.
* **Effects stop at the clip's frame.** Glows, borders and similar effects that would extend past
  the clip are cropped to it.
* Desktop only: there is no OFX on the web build. Developed and tested on Windows x64; the macOS and
  Linux loaders are implemented but untested with commercial plug-ins.
* Some plug-ins rely on host features that are not implemented (analysis passes, multi-threaded
  frame rendering, custom interpolation, host-side clip caching). Those plug-ins may load with
  reduced functionality or report errors in the loader's `errors` list.
* Plug-ins are loaded on a dedicated thread with a 64 MiB stack (Sapphire needs more than the
  1–2 MiB of the threads that ask). Helper programs plug-ins leave running (Sapphire's
  `preset-browser.exe`, about 340 MB each) are stopped when FilmCraft exits normally.
* Boris FX Sapphire occasionally (about 1 start in 10 while another Sapphire host is running)
  crashes while describing its first effect, a race in its preset-browser pre-launch; EffectCraft
  shows the same. The blocklist only skips a plug-in after two deaths in a row; setting
  `prelaunch_preset_browser: 0` in Sapphire's `s_config.text` turns the pre-launch off.

## How it fits together

* **`crates/ofx`** (`filmcraft-ofx`, layer L3): the C ABI transcribed from the OpenFX headers,
  property sets, parameter storage and the mapping above, clip images and pixel conversion (OFX is
  y-up, rows are flipped on the way in and out), effect instances and render contexts, the host
  suites (property, parameter, image effect, memory, multithread, message, progress, timeline,
  interact), bundle discovery and the crash blocklist. It is built only with the `openfx` feature.
* **Engine / apps:** the commands are `effects.plugins.list`, `effects.plugins.loadOfx` and
  `effects.plugins.scanOfx`; the desktop app and `filmcraft-cli` (headless commands and `mcp`)
  run the scan at start-up, before any project is opened, unless `FILMCRAFT_NO_OFX=1`.

## Testing the host

`examples/ofx/fc-test-ofx` is a small dependency-free OFX plug-in (Invert, Echo, Border, Mix and an
effect with every parameter type) that exercises the host; its effects appear as **FC Test**
effects. It is a standalone cargo workspace, not part of the main one. The integration tests in
`crates/ofx/tests` load it; build it into a bundle first:

```sh
cargo build --release --manifest-path examples/ofx/fc-test-ofx/Cargo.toml
# lay out the bundle (Windows shown; use Contents/Linux-x86-64 and libfc_test_ofx.so on Linux)
mkdir -p target/ofx-test/fc-test-ofx.ofx.bundle/Contents/Win64
cp examples/ofx/fc-test-ofx/target/release/fc_test_ofx.dll target/ofx-test/fc-test-ofx.ofx.bundle/Contents/Win64/fc-test-ofx.ofx
FC_TEST_OFX_BUNDLE=$PWD/target/ofx-test/fc-test-ofx.ofx.bundle cargo test -p filmcraft-ofx
```

Without `FC_TEST_OFX_BUNDLE` the integration tests print a note and pass without testing anything
(the unit tests always run). You can also put the bundle in a standard OFX folder or on
`OFX_PLUGIN_PATH` to see the FC Test effects in an `openfx` build of the app.
