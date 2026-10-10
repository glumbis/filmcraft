# FilmCraft with OpenFX plug-ins

This fork of [FilmCraft](https://github.com/storytold/filmcraft) can run **OpenFX (OFX)
plug-ins**, the plug-in standard used by DaVinci Resolve, Nuke, Vegas and Natron, as ordinary video
effects: they show up in the Effects panel, their settings appear in Effect Controls, and they take
keyframes.

| Plug-in | Status |
|---|---|
| RE:Vision **ReelSmart Motion Blur** (RSMB, RSMB Pro, RSMB Pro Vectors) | ✅ loads and renders |
| RE:Vision **Twixtor** (Twixtor, Twixtor Pro, Vectors In, Motion Vectors Create) | ✅ slow motion and speed ramps |
| Boris FX **Sapphire** (291 effects) | ✅ loads and renders |
| Other OFX plug-ins | Should work if they have a CPU render path; [report how it goes](https://github.com/glumbis/filmcraft/issues) |

The OFX support is **unofficial**. It may be proposed upstream later; until then it lives here.
Please don't report problems with it to the FilmCraft maintainers or to the plug-in vendors.

## Install (Windows)

1. **Download** `FilmCraft-OFX-windows.zip` from the
   [latest release](https://github.com/glumbis/filmcraft/releases/latest) and unzip it.
2. **Double-click `Install FilmCraft.bat`.** It installs the app for your user (no administrator
   rights) into `%LOCALAPPDATA%\Programs\FilmCraft-OFX`, and adds **"FilmCraft (OFX)"** to the Start
   menu and desktop.
3. **Install your plug-ins** with their own installers, choosing the **OFX** version (not the
   Premiere or After Effects one). They go into `C:\Program Files\Common Files\OFX\Plugins`, where
   FilmCraft finds them automatically.
4. **Start "FilmCraft (OFX)".** It isn't code-signed, so Windows SmartScreen may warn the first
   time: click *More info* ▸ *Run anyway*. Your plug-ins appear in the **Effects** panel under
   **Video Effects**, in their own groups (for example *REVision Effects*, *Sapphire Stylize*),
   plus **FC Test**, a tiny test plug-in that comes with the download. Drag an effect onto a clip
   or double-click it, then adjust it in **Effect Controls**.

To remove it: run *Uninstall FilmCraft.bat* in the install folder
(`%LOCALAPPDATA%\Programs\FilmCraft-OFX`). Your projects, settings and plug-ins stay.

**Licences:** FilmCraft doesn't include or unlock any plug-in. Without a licence they run as
trials with a watermark; activate them with the vendor's own tools (RE:Vision Activate, the Boris FX
Hub).

**Want to check it works without a commercial plug-in?** The installer already sets up
`fc-test-ofx.ofx.bundle`, a tiny test plug-in, in its `OFX` folder and adds that folder to your
`OFX_PLUGIN_PATH`. Apply one of the **FC Test** effects (Video Effects ▸ FC Test). Without the
installer, put the bundle in the OFX folder above, or choose **File ▸ Load OpenFX Plug-in…** and
pick the `.ofx` file inside its `Contents\Win64` folder.

## Tips

- **Twixtor starts at Speed 100 %**, which changes nothing: set Speed to e.g. 50 for half-speed
  slow motion, or keyframe it for speed ramps.
- **Effects are confined to the clip's frame**, as in Premiere: the plug-in sees transparent
  padding around the clip, but anything it draws outside the frame is cropped.
- Everything renders on the **CPU**, so heavy effects are slow at high resolution.
- Temporal plug-ins (Twixtor, RSMB) read other frames of the clip, which makes them slower still.
- "Layer" inputs of a plug-in are a choice of video track.
- If a plug-in crashes FilmCraft while loading twice in a row, it's skipped automatically from
  then on (one interrupted start-up, such as closing FilmCraft while it loads, isn't enough). To start
  without any OFX plug-ins, set the environment variable `FILMCRAFT_NO_OFX=1`.

More detail (supported features, limits, how it works): [docs/ofx.md](docs/ofx.md).

## Building it yourself

You need [Rust](https://rustup.rs/) 1.95 or newer (and on Windows the Visual Studio C++ build
tools). OFX support is an opt-in feature:

```sh
cargo run --release -p filmcraft --features openfx
```

Release builds use the fonts from [storytold/craft-fonts](https://github.com/storytold/craft-fonts);
see [docs/contributing.md](docs/contributing.md#building-with-craft-fonts). Without them FilmCraft
builds and runs with its bundled and system fonts.

## Credits

Built by [@glumbis](https://github.com/glumbis), mostly written with Claude Code. FilmCraft itself
is by the [FilmCraft contributors](https://github.com/storytold/filmcraft). The OpenFX API headers
are © the OpenFX contributors (BSD-3-Clause).
