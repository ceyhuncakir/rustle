# Rustle

**Hold a key, speak, and clean text appears wherever you are typing.**
Rustle is a private dictation app for Windows, macOS and Linux. Your voice
is turned into text on your own computer, in any of 25 languages, and a
cleanup pass makes it read as if you had typed it: punctuation, no "um"s,
changes of mind resolved.

- **Private by default.** Speech recognition runs locally. Nothing you say
  leaves the machine unless you choose a cloud model for the cleanup.
- **25 languages, detected on their own.** Switch languages whenever you
  like without touching a setting; the text comes out in the language you
  spoke.
- **Works in every app.** Editors, browsers, chat, terminals. A small island
  at the bottom of the screen shows when it is listening, thinking and done.
- **Fast.** A five-second dictation is recognised in about a tenth of a
  second on a desktop CPU, and faster still on a graphics card.
- **Free and open source**, MIT licensed.

## Install

Download the installer for your system from the
[latest release](https://github.com/ceyhuncakir/rustle/releases/latest):

| System | File | |
|---|---|---|
| **Windows** 10 and 11 | `Rustle_…_x64-setup.exe` | Installs for your user; no admin rights needed |
| **macOS** 12 or later, Apple Silicon | `Rustle_…_aarch64.dmg` | Drag Rustle into Applications |
| **Linux** | `Rustle_…_amd64.AppImage` | Runs anywhere; updates itself |
| Debian, Ubuntu | `Rustle_…_amd64.deb` | `sudo apt install ./Rustle_…_amd64.deb` |
| Fedora, openSUSE | `Rustle-…x86_64.rpm` | `sudo dnf install ./Rustle-…x86_64.rpm` |

Intel Macs are not supported: the speech engine no longer ships for them.

<details>
<summary><b>"Unidentified developer" or "Windows protected your PC"?</b></summary>

Until the releases are code-signed, both systems warn the first time:

- **macOS:** right-click Rustle in Applications and choose **Open**, then
  **Open** again. Or run `xattr -dr com.apple.quarantine /Applications/Rustle.app`.
- **Windows:** click **More info**, then **Run anyway**.

</details>

### First launch

A short setup wizard walks you through it:

1. **Microphone.** Pick one and check that the level meter moves.
2. **Speech model.** Pick the model (Parakeet v3 for 25 languages, or v2 for
   English only) and whether it runs on the CPU or the graphics card. It
   downloads once: about 670 MB for the CPU, 2.5 GB for the graphics card.
   An interrupted download picks up where it stopped.
3. **Shortcut.** Press the one you want, or keep the default.
4. **Paste test.** Rustle types into a test field, so you know it can reach
   your other apps. On macOS this is where it asks for Accessibility access.
5. **Cleanup.** Choose where it runs (Ollama on your machine, a cloud
   provider, or not at all) and which model: any model you have in Ollama or
   your provider offers, or type one in (see [Cleanup](#cleanup)).

Then hold the shortcut, speak, and let go. Everything the wizard sets can
be changed later in the settings window: the speech model and where it
runs under Voice, the cleanup provider and model under Cleanup.

## Using it

| | |
|---|---|
| **Hold** the shortcut | Records while held, and pastes when you let go |
| **Tap** the shortcut | Starts recording; tap again to stop |
| Default shortcut | **Ctrl+Alt+Space**; **Option+D** on a Mac; **Super+D** on GNOME |
| Cancel a take | **Super+Ctrl+Escape** on GNOME |

The tray icon opens the settings window, where you can change the
shortcut, the microphone, the cleanup model and everything else.

### Languages

Rustle recognises these 25 languages and works out which one you are
speaking, dictation by dictation:

Bulgarian, Croatian, Czech, Danish, Dutch, English, Estonian, Finnish,
French, German, Greek, Hungarian, Italian, Latvian, Lithuanian, Maltese,
Polish, Portuguese, Romanian, Russian, Slovak, Slovenian, Spanish, Swedish
and Ukrainian.

The cleanup keeps each dictation in the language it was spoken in. To
always write in one language instead, pick it under Settings → Cleanup →
Output language, and Rustle translates the rest. If you mostly speak one
or two languages, naming them in the config file (`languages = ["de",
"en"]`) helps the cleanup model stay on track.

## Cleanup

Recognition turns your voice into words; the cleanup pass decides what you
meant by them. It adds punctuation and paragraphs, removes fillers and
false starts, puts spoken URLs and email addresses back together, and
resolves a change of mind ("ship it Monday, no wait, Tuesday" becomes
"Ship it Tuesday."). It never answers or acts on what you dictated.

It runs where you choose:

- **Locally, through [Ollama](https://ollama.com).** Nothing leaves your
  machine. Install Ollama, pull a model, and pick it from the list: any
  model you have pulled works. `qwen3:14b` is the one the cleanup is tuned
  and tested against; `qwen3:8b` is a smaller choice if memory is tight. On
  Linux, `scripts/install-ollama.sh` sets up a rootless Ollama for you.
- **With a cloud provider:** Anthropic, OpenAI, OpenRouter, DeepSeek, or any
  OpenAI-compatible endpoint, including local servers such as llama.cpp and
  LM Studio. Pick a model from the provider's own list, or type any model
  name it serves. API keys are kept in your system's keychain, never in a
  file.
- **Not at all**, if you want the raw transcript.

## Privacy

- The microphone records only while you hold, or have tapped, the
  shortcut, and the audio is never written to disk.
- Speech recognition always runs on your computer.
- Transcripts go to a cloud provider only if you chose one for the cleanup.
- Rustle keeps no record of what you say unless you switch learning on
  (below), and what you dictate never appears in its logs.
- Apart from the provider you choose, Rustle goes online only to download
  the speech model once and to check for updates daily, which you can turn
  off.

## Speed and graphics cards

Recognition runs on the CPU, or on the graphics card when that is faster:

- **CPU:** about 40 times faster than real time on a desktop processor.
- **Graphics card:** about 140 times real time on an RTX 4090. AMD, Intel
  Arc and NVIDIA cards work through Vulkan on Linux and Direct3D 12 on
  Windows, and Macs use their built-in GPU. Cards need 4 GB of memory.
  Graphics built into a PC's processor are left alone, because the CPU is
  faster there.

Recognition is the quick part. Most of the wait after you let go is the
cleanup model, so a smaller or faster model there makes the biggest
difference. `rustle gpu` shows which card was found and what Rustle will
use.

## Linux notes

Rustle works on GNOME, KDE, Hyprland, sway and other desktops, on Wayland
and X11. Some desktops need a little help:

- **GNOME** gives no ordinary app a way to float above other windows or to
  paste into them, so Rustle comes with a GNOME Shell extension that does
  both. The setup wizard installs it; log out and back in once so GNOME
  loads it.
- **KDE and Hyprland** ask you to confirm the shortcut once, in their own
  dialog.
- **sway, river and niri** have no shortcut dialog, so bind a key in the
  compositor to `rustle hotkey`:

  ```sh
  # sway: hold to talk
  bindsym --no-repeat Ctrl+Alt+space exec rustle hotkey down
  bindsym --release Ctrl+Alt+space exec rustle hotkey up
  # Hyprland
  bind = CTRL ALT, space, exec, rustle hotkey down
  bindr = CTRL ALT, space, exec, rustle hotkey up
  # river
  riverctl map normal Control+Alt Space spawn 'rustle hotkey down'
  riverctl map -release normal Control+Alt Space spawn 'rustle hotkey up'
  # niri (press only, so tap to start and tap to stop)
  Mod+Space repeat=false { spawn "rustle" "hotkey" "toggle"; }
  ```

  `rustle hotkey cancel` drops the take in progress.
- **On Wayland outside GNOME**, pasting needs `dotool`, or `ydotool` with
  its `ydotoold` service running; the setup wizard checks for one. The
  island needs `gtk-layer-shell` to float above windows
  (`libgtk-layer-shell0` on Debian and Ubuntu) and falls back to a plain
  window without it.

On GNOME you can also run Rustle without any window, as a background
service: `systemctl --user enable --now rustle`. Only one copy dictates at a
time.

### Build from source

```sh
git clone https://github.com/ceyhuncakir/rustle && cd rustle
scripts/install-app.sh      # builds and installs ~/.local/bin/rustle, the service and the GNOME extension
```

It needs [rustup](https://rustup.rs) (the Rust version in
`rust-toolchain.toml` is fetched for you), Node.js with pnpm, and your
distribution's WebKitGTK development packages. `--cuda` builds for NVIDIA's
CUDA instead of WebGPU: slightly faster on recognition, but it needs CUDA 12
and cuDNN 9. `--cpu` leaves the GPU out.

## Settings and the config file

The settings window covers the everyday options. Everything is also in a
commented `config.toml`, which the settings window edits in place:

| System | Config | Models and history |
|---|---|---|
| Linux | `~/.config/rustle` | `~/.local/share/rustle` |
| macOS | `~/Library/Application Support/rustle` | the same folder |
| Windows | `%APPDATA%\rustle` | `%LOCALAPPDATA%\rustle` |

The parts worth knowing:

- `[cleanup] dictionary`: names and jargon the recogniser keeps getting
  wrong.
- `[cleanup.app_rules]`: a tone per app, keyed by the name `rustle context`
  prints with that app focused.
- `[cleanup] style`: `light`, `balanced` or `tidy`.
- `[cleanup] languages` and `output_language`: see [Languages](#languages).
- `[stt] provider`: `auto` uses the graphics card when it is faster, `gpu`
  insists, `cpu` never tries.

A value that does not fit is skipped on its own, and `rustle doctor` says
which.

## Learning your vocabulary

Off by default. Switched on (Settings → Learning, or `rustle learning on`),
Rustle keeps your dictations in a local database and every so often works
out the names and jargon a general recogniser gets wrong, plus one sentence
about how you write. Both go into the cleanup prompt. A term must appear at
least twice in your own dictations before it is used, and
`rustle vocab --forget TERM` blocks one for good. **Forget all** in the
settings, or `rustle history --clear`, deletes everything stored.

## Command line

```
rustle                          the app, with its tray icon (the first launch opens the setup wizard)
rustle doctor                   check every moving part and say what is wrong
rustle gpu                      graphics cards, and whether recognition can use one
rustle devices                  microphones
rustle dictate -s 5             record five seconds, recognise, clean up and paste
rustle hotkey down|up|toggle|cancel   drive the running app from a key binding (Linux)
rustle context                  what the desktop reports as the focused app
rustle config [--edit]          the config file
rustle models status|download   the speech model files
rustle learning on|off|status · rustle vocab [--forget X] · rustle learn · rustle history [--clear]
rustle eval                     run the cleanup test cases against the configured model
rustle --headless               the engine without windows, for the GNOME background service
```

## Updates

Rustle checks for a new release once a day and tells you when one is out
(Settings → Updates, where you can also turn the check off). On Windows,
macOS and the AppImage it updates itself; `.deb` and `.rpm` installs get a
link to the new release. Every update is verified against Rustle's signing
key before it is installed.

## Troubleshooting

- `rustle doctor`, or **Check setup** under Settings → Status, checks the
  microphone, the speech model, the graphics card, the shortcut, pasting
  and the cleanup model, and names whatever is broken.
- **Copy diagnostics** under Settings → About puts a full report on the
  clipboard, ready for a bug report.
- On GNOME, if nothing happens when you press the shortcut, log out and back
  in once: GNOME only loads a newly installed extension at login.

## Status

Rustle is new. What has been checked where:

| Platform | State |
|---|---|
| Linux, GNOME on Wayland | Used daily |
| Linux, KDE, Hyprland, sway, X11 | The pieces are tested one by one; not yet a full session on each |
| Windows | Built and tested automatically; not yet tried on a real machine |
| macOS | Built and tested automatically; not yet tried on a real machine |

Known limits:

- Change-of-mind phrases ("no wait", "scratch that") are recognised in
  English and Dutch so far. In other languages the rest of the cleanup works
  as usual.
- Only plain text survives on the clipboard across a paste; a copied image
  is lost.
- On GNOME, dictating with the Activities overview open pastes into its
  search field.
- GNOME extensions can break with new GNOME versions. Rustle's is declared
  for GNOME 47 to 51 and has run on 48.

## Development

```sh
pnpm install
cargo test --workspace                       # 300+ tests, no models needed
pnpm tauri dev -- --features webgpu          # run the app from source (or --features cuda)
cargo run -p rustle-desktop --example island # drive the live GNOME island over D-Bus
scripts/nested-shell.sh                      # a throwaway GNOME Shell for extension work
```

CI runs formatting, clippy and the tests on Linux, Windows and macOS. The
release workflow builds every installer; see
[docs/releasing.md](docs/releasing.md). Before a release, work through
[docs/qa-checklist.md](docs/qa-checklist.md).

### How it fits together

```
┌─ GNOME Shell extension (GNOME only, inside the Shell) ────────────┐
│  island · focused window · pasting · shortcut                     │
└──────────────── dev.ceyhun.Rustle.Island (session bus) ───────────┘
┌─ rustle (Rust + Tauri) ───────────────────────────────────────────┐
│  engine · microphone · Parakeet on ONNX Runtime · cleanup pass    │
│  tray · settings window · setup wizard                            │
│  per-desktop parts: GNOME (D-Bus) · Windows · macOS · X11 ·       │
│  Wayland (layer-shell overlay, shortcut portal, paste helpers)    │
└───────────────────────────────────────────────────────────────────┘
```

On GNOME the island has to live inside the Shell: Mutter lets no ordinary
app draw an always-on-top, click-through overlay, and only the Shell can
see the focused window or paste into it. Everywhere else the app draws the
island in a window of its own. The engine is one state machine with every
platform concern behind a trait, so the whole dictation path is tested
with fakes.

```
crates/rustle-core/     engine, cleanup prompts and passes, providers, config, history, learning, keys
crates/rustle-audio/    microphone capture
crates/rustle-stt/      Parakeet TDT on ONNX Runtime, model download, GPU detection
crates/rustle-desktop/  island, shortcut, focus and paste for each desktop
src-tauri/              the app: tray, windows, settings commands, command line
ui/                     the island (canvas), settings window and setup wizard (React)
extension/              the GNOME Shell extension
eval/cases.toml         cleanup test cases (`rustle eval`)
docs/                   release process and the manual test checklist
flowd/, tests/*.py      the earlier Python version, kept for reference
```

## License

MIT. Rustle uses NVIDIA's Parakeet TDT 0.6B v3 model (CC-BY-4.0) and
other open-source components; see [THIRD_PARTY.md](THIRD_PARTY.md).
