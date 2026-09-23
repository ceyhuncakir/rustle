# Flow

Local, offline dictation: hold a key, talk, and cleaned-up text appears in
whatever app you are in, with a floating island showing what is happening.

Recognition runs on your machine (NVIDIA Parakeet TDT 0.6B v3, English and
Dutch, detected automatically). A cleanup pass turns the raw transcript into
text worth pasting: punctuation, filler removal, self-corrections resolved,
per-app tone. That pass runs on a local model through Ollama, on a cloud
provider of your choice, or not at all.

| | |
|---|---|
| Floating island | Renders above every window, never takes focus or eats a click |
| Push-to-talk | One shortcut gives both hold-to-talk and tap-to-toggle |
| Recognition | Parakeet TDT v3 on onnxruntime, on the CPU or any graphics card: ~40x realtime on a CPU, ~140x on an RTX 4090 |
| Cleanup | Local Qwen3 via Ollama, or Anthropic / OpenAI / OpenRouter / DeepSeek / any OpenAI-compatible endpoint |
| Focus context | The focused app's identity and title feed the cleanup model, so tone adapts per app |
| Learning | Optional, off by default: picks up your jargon and register over time |
| Settings | A preferences window, a first-run wizard, and a tray icon |
| Diagnostics | `flow doctor` checks every moving part and says what is wrong |

## Status

The Rust + Tauri version replaces the earlier Python daemon. What is verified
where:

| Platform | State |
|---|---|
| Linux, GNOME Wayland | **Works.** The Shell extension draws the island and delivers the hotkey; the app talks to it over D-Bus. Daily-driver tested. |
| Linux, X11 | Built, not yet verified on a real session. GNOME on X11 uses the extension when it runs. |
| Linux, KDE / Hyprland / other Wayland | Built without a global hotkey yet; pasting needs `dotool` or `ydotool`. Planned for a later milestone. |
| Windows | Built through CI, not yet verified on a machine. |
| macOS | Built through CI, not yet verified on a machine; non-activating panel and permission prompts still to come. |

Every crate's tests, the recognition parity suite and the 28-case cleanup
evaluation pass on the Linux development machine.

## Install

### Linux, GNOME (today)

```sh
git clone https://github.com/ceyhuncakir/flow && cd flow
scripts/install-app.sh            # builds, installs ~/.local/bin/flow, the service, the extension
```

Recognition runs on the graphics card when that is faster: AMD, Intel Arc
and NVIDIA cards through WebGPU on Vulkan, which needs nothing beyond the
graphics driver (on Fedora, `mesa-vulkan-drivers` for AMD and Intel). A card
built into the processor is left alone, because the CPU is faster there (an
Intel UHD 770 managed 6x realtime against the CPU's 36x); pick "GPU only" in
the settings to use it anyway. Cards need 4 GB of memory. `flow gpu` shows
what was found and what it will use.

On NVIDIA, `scripts/install-app.sh --cuda` builds for CUDA instead: about 20%
faster on recognition, which is a few milliseconds per dictation, but it
needs CUDA 12 and cuDNN 9 and covers the RTX 20 to 40 series only. Flow finds
those libraries on the loader path, under `/usr/local/cuda` and in pip's
`nvidia-*` wheels (`pip install --user nvidia-cudnn-cu12`). `--cpu` leaves
the GPU out.

Log out and back in once so GNOME loads the extension (Wayland cannot
hot-load one). The deb, rpm and AppImage carry the extension too; the setup
wizard installs it for you, and the same log-out applies. Then either:

```sh
systemctl --user start flow       # headless: the extension is the whole UI
flow                              # or the tray app with the settings window and wizard
```

Only one of them dictates at a time; the second says so and leaves the
shortcut to the first.

Press **Super+D** and talk. Hold it and it stops when you let go; tap it and
it stops on the next tap. **Super+Ctrl+Escape** cancels. Elsewhere the
shortcut is **Ctrl+Alt+Space** (**Option+D** on a Mac), and the settings
window changes it on every desktop.

Cleanup is optional. For the local model, install Ollama and pull one:

```sh
scripts/install-ollama.sh         # rootless Ollama + qwen3:14b
```

or pick a cloud provider in the settings window. API keys go in the system
keyring, never in the config file.

### Windows and macOS

Installers are produced by the release workflow. They are not yet verified on
real machines; treat them as previews until the platform milestones below are
done. They recognise speech on the GPU through WebGPU: Direct3D 12 on
Windows, and on the Mac the Apple Silicon GPU through Metal. Intel Macs are
not supported, because ONNX Runtime no longer ships for them.

## Why it is built this way

GNOME on Wayland rules out the obvious designs, so the split is forced:

- **Mutter has no `wlr-layer-shell`.** No ordinary client can place an
  always-on-top, click-through overlay. On GNOME the island therefore lives
  *inside* the Shell as an extension. Elsewhere the app draws it in a window
  of its own.
- **`wtype` does not work on GNOME.** It needs `zwp_virtual_keyboard_v1`,
  which Mutter does not expose. KDE does not either.
- **`ydotool` needs root.** It writes to `/dev/uinput`.
- **Only the Shell can see the focused window** on Wayland, and that context
  is exactly what lets the cleanup model adapt its tone per app.

```
┌─ GNOME Shell extension (GJS, in Mutter's process) ────────────────┐
│  island UI · focus context · text injection · hotkeys             │
└──────────────────── ai.flow.Island (session bus) ─────────────────┘
┌─ flow (Rust + Tauri) ─────────────────────────────────────────────┐
│  engine · audio capture · Parakeet on onnxruntime · cleanup pass  │
│  tray · settings window · first-run wizard                        │
│  per-desktop backends: GNOME (D-Bus) · Windows · macOS · X11 ·    │
│  Wayland (layer-shell + paste helpers)                            │
└───────────────────────────────────────────────────────────────────┘
```

The engine is one state machine driven by a channel, with every platform
concern behind a trait, so the whole dictation path is tested with fakes.

## Layout

```
crates/flow-core/      engine, cleanup prompts and passes, providers, config, history, learning, secrets
crates/flow-audio/     microphone capture (cpal + rubato)
crates/flow-stt/       Parakeet TDT on onnxruntime (ort), model download, parity tests
crates/flow-desktop/   overlay / hotkey / focus / paste per desktop; GNOME over D-Bus
src-tauri/             the app: tray, windows, settings commands, CLI subcommands
ui/                    overlay pill (canvas), settings and first-run (React)
extension/             the GNOME Shell extension
eval/cases.toml        the cleanup evaluation cases (`flow eval`)
tests/fixtures/stt/    recognition parity goldens (WAVs regenerate from scripts/stt-golden.py)
docs/qa-checklist.md   the per-platform manual checklist
flowd/, tests/*.py     the previous Python implementation, kept until the GNOME path has been the daily driver for a while
```

## Command line

```
flow                     the tray app (first launch opens the setup wizard)
flow --headless          engine only, for the GNOME user service
flow doctor              check every moving part
flow dictate -s 5        record five seconds, recognise, clean, paste
flow context             what the desktop reports as the focused app
flow devices             microphones
flow gpu                 graphics cards, and whether recognition can use one
flow config [--edit]     the config file
flow models status|download|import-hf
flow eval                run the cleanup cases against the configured model
flow learning on|off|status · flow vocab [--forget X] · flow learn · flow history [--clear]
```

## Configuration

`config.toml` in `~/.config/flow` (Linux), `~/Library/Application Support/flow`
(macOS) or `%APPDATA%\flow` (Windows); models and the history database live
in `~/.local/share/flow`, the same folder on macOS, and `%LOCALAPPDATA%\flow`
on Windows. A value in the file that does not fit is skipped on its own and
`flow doctor` names it. The settings window edits the file in place
and keeps the comments; the file is still the nicer way to set per-app rules
and the dictionary. The parts worth knowing:

- `[cleanup] dictionary` - names and jargon the recogniser mangles.
- `[cleanup.app_rules]` - per-application tone, keyed by the app identifier
  `flow context` prints (a WM class on GNOME, an app name elsewhere).
- `[cleanup] style` - `light`, `balanced` or `tidy`.
- `[cleanup] resolve_intent` - the change-of-mind rule, the only one that
  deletes content.
- `[cleanup] output_language` - `same`, `en` or `nl`.
- `[stt] provider` - `auto` uses the graphics card when `flow gpu` says it
  is faster than the CPU; `gpu` insists (`cuda`, its old name, still works);
  `cpu` never tries. For a CUDA build, CUDA libraries somewhere unusual can
  be named in `FLOW_CUDA_LIBS` (a path list).
- `[desktop] overlay` - `auto`, `window` or `off` (ignored on GNOME).
- `[desktop] hotkey` - the shortcut: `Ctrl+Alt+Space` by default, `Option+D`
  (`Alt+D`) on macOS. On GNOME it lives in the extension's settings instead,
  and the settings window changes it there.

## What the cleanup pass does

Parakeet turns audio into words, then the cleanup model decides what you
meant by them. Ordinary dictation costs one model call; a second, conditional
pass runs only when the transcript contains a retraction cue ("no wait",
"scratch that", "nee wacht"), and a third only when translating.

The polish pass fixes punctuation and capitalisation, removes disfluencies
and filler uses of "like" and "you know" while keeping them where they mean
something, repairs the fragments that leaves behind, assembles spoken URLs
and emails, resolves a change of mind so the output reads as if you had only
ever said the final version, keeps discourse words and slang, and never
answers or acts on what you dictated. An output more than 1.5x longer than
the input, or containing a code block, is rejected and the raw transcript is
pasted instead.

`flow eval` pins all of that against the live model with 28 cases, eight of
them verbatim from real dictations where the failure showed up and four in
Dutch. Reasoning is off by default: on that corpus it scored the same while
taking 3-21 s instead of 0.1-0.8 s.

## Learning your vocabulary (off by default)

Switched on, Flow stores your dictations locally in SQLite and periodically
mines two things from them with the cleanup model: the names a general
recogniser gets wrong, and one sentence describing how you write. Both feed
back into the prompt. Every mined term must appear at least twice in your
own history before it is kept, and `flow vocab --forget` blocks a term for
good. While learning is off, nothing you dictate is stored. What you
dictate never goes to the log either, unless you ask for it with `-v`.

## Development

```sh
pnpm install
cargo test --workspace                     # 200+ tests, no models needed
pnpm tauri dev -- --features webgpu        # the app, with GPU recognition (or cuda)
cargo run -p flow-desktop --example island # drive the live GNOME island over D-Bus
scripts/nested-shell.sh                    # a throwaway GNOME Shell for extension work
.venv/bin/python scripts/stt-golden.py     # regenerate recognition fixtures and goldens
```

CI runs format, clippy and tests on Linux, Windows and macOS, and the release
workflow builds installers for all three with `tauri-action`.

## Third-party components

See [THIRD_PARTY.md](THIRD_PARTY.md). Flow itself is MIT licensed.

## Known limits

- Only `text/plain` is preserved across the paste; an image on the clipboard
  is lost.
- On GNOME, dictating with the Overview open pastes into the Overview search
  entry.
- GNOME extensions break across major GNOME releases. The extension is declared
  for GNOME 47 to 51, but has only run on 48.
- English and Dutch by choice; Parakeet v3 covers 25 European languages and
  a third one is transcribed rather than rejected.
- The cleanup model adds latency proportional to output length. Recognition
  is effectively free; the model is the slow part.
