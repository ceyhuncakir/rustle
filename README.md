# Flow

A local, offline dictation stack for Fedora + GNOME Wayland: hold a key, talk,
and cleaned-up text appears in whatever app you are in — with a floating island
showing what is happening.

Status: **phases 1-3 done** — island, daemon, hotkey, recognition, cleanup and
app packaging all work end to end. Phase 4 (settings GUI) is not started.

## What works today

| | |
|---|---|
| Floating island | Renders above every window, never takes focus or eats a click |
| Six states | `idle`, `listening`, `thinking`, `inserting`, `error`, `hidden`, with morph animations |
| Live waveform | Cairo-drawn bars driven by real microphone amplitude |
| Push-to-talk | One shortcut gives both hold-to-talk and tap-to-toggle |
| Recognition | Parakeet TDT 0.6B v3 — English + Dutch, detected automatically, ~0.05s |
| Cleanup pass | Local Qwen3 via Ollama: punctuation, filler removal, self-corrections, per-app tone |
| Focus context | Reads the focused app's WM class and title, and feeds it to the cleanup model |
| Text injection | Pastes into any app — verified cross-process into GNOME Text Editor |
| App entry | "Flow Dictation" in the Applications grid; on-demand, never autostarted |
| Learning | Optional, off by default: picks up your jargon and register over time |
| Settings | GTK4/libadwaita window: models, providers, keys, language, learning |
| Diagnostics | `flow doctor` checks every moving part and says what is wrong |

## Install

```sh
uv venv --python 3.13 --system-site-packages   # system PyGObject must be visible
uv pip install -e '.[gpu]'                     # or '.[cpu]'
scripts/install-app.sh                         # binary, icon, .desktop, user service
scripts/install-ollama.sh                      # optional, no root; pulls the cleanup model
scripts/install.sh                             # link the Shell extension, then LOG OUT
```

Wayland cannot load a new Shell extension without a logout — there is no
`Alt+F2` `r`. After logging back in, `flow doctor` should be green.

Then press **Super+D** and talk. Hold it and it stops when you let go; tap it
and it stops on the next tap.

## Why it is built this way

GNOME 48 on Wayland rules out the obvious designs, so the split is forced:

- **Mutter has no `wlr-layer-shell`.** No ordinary client can place an
  always-on-top, click-through overlay. The island therefore has to live
  *inside* the Shell as an extension. A Tauri or Electron window cannot do it.
- **`wtype` does not work on GNOME.** It needs `zwp_virtual_keyboard_v1`, which
  Mutter does not expose to clients.
- **`ydotool` needs root.** It writes to `/dev/uinput`, which means a udev rule
  and a privileged daemon.
- **Only the Shell can see the focused window.** On Wayland a client cannot ask
  what else is on screen — but that context is exactly what lets the cleanup
  model adapt its tone per app.

Running inside the Shell solves all four at once, using Mutter's own virtual
input device, which needs no privileges at all.

```
┌─ GNOME Shell extension (GJS, in Mutter's process) ─────────┐
│  island UI · focus context · text injection · hotkeys      │
└──────────────────── ai.flow.Island (session bus) ──────────┘
┌─ flowd (Python, systemd --user) ───────────────────────────┐
│  audio capture · VAD · Parakeet STT · Ollama cleanup pass  │
└────────────────────────────────────────────────────────────┘
┌─ GTK4 + libadwaita settings GUI ───────────────────────────┐
│  models · hotkeys · dictionary · history · per-app rules   │
└────────────────────────────────────────────────────────────┘
```

The daemon stays an ordinary unprivileged user process. It owns audio and the
models; it owns no pixels.

## Layout

```
extension/      GNOME Shell extension (the only privileged-ish part)
  island.js       the pill: states, morph animations, positioning
  visualizer.js   Cairo waveform / dots / check-mark, one 60fps draw loop
  injector.js     clipboard + virtual-device paste, focus context
  extension.js    D-Bus surface, lifecycle
flowd/          the daemon (phase 2+)
  island.py       D-Bus client for the island
scripts/
  install.sh         symlink the extension into place
  nested-shell.sh    run it in an isolated Shell, headless or windowed
  capture.py         screenshot every island state
  test-injection.py  end-to-end proof that pasting works
  demo.py            full fake dictation cycle
  flowctl            one-shot CLI over the same D-Bus surface
```

## Development loop

Wayland cannot restart the Shell in place — there is no `Alt+F2` `r` — so a
changed extension needs either a logout or a second Shell. `nested-shell.sh`
runs the second Shell, on its own Wayland socket and its own D-Bus, so nothing
can disturb the real session:

```sh
scripts/nested-shell.sh                          # visible window, poke at it
FLOW_DEV=1 scripts/nested-shell.sh scripts/capture.py        # shoot all states
FLOW_DEV=1 scripts/nested-shell.sh scripts/test-injection.py # prove injection
```

`FLOW_DEV=1` turns on Mutter's unsafe mode so the screenshot API can be
scripted. It is read from the environment by the extension itself, so it leaves
no trace in dconf — which a nested Shell shares with the real session.

To use it for real:

```sh
scripts/install.sh      # then log out and back in; Wayland cannot hot-reload
flowctl show
flowctl state listening
flowctl insert "hello"
```

## D-Bus contract

`ai.flow.Island` at `/ai/flow/Island` — the whole surface between the two
halves. `flowctl` and `flowd` are peers, not layers.

| Member | Purpose |
|---|---|
| `SetState(s)` | `idle` · `listening` · `thinking` · `inserting` · `error` · `hidden` |
| `SetText(s)` | live partial or final transcript in the pill |
| `PushLevel(d)` | one amplitude sample, 0..1, at frame rate |
| `InsertText(s)` | paste into the focused window |
| `GetFocusContext()` | `{app, title, role}` of the focused window |
| `State` | current state, readable property |
| `HotkeyPressed(s)` / `HotkeyReleased()` / `CancelRequested()` | signals, phase 2 |

## Roadmap

**Phase 4 — the GUI.** GTK4 + libadwaita: model picker, hotkey binding,
dictionary editor, history with search, per-app rules, and an edit-learning
loop that feeds your corrections back into the prompt. Also still open: a
Claude API cleanup backend behind a config flag, and dictation history in
SQLite.

## Settings window

`flow gui`, or the "Flow Dictation" icon in the Applications grid. GTK4 and
libadwaita, so it matches the rest of the desktop.

It covers the whole stack: start/stop with the live hotkey shown, the speech
model (with a note when picking one means a download), CPU/GPU, microphone,
the cleanup provider and model, editing style, output language, and the
learning toggle with what it has picked up.

Every control writes straight to `config.toml`, keeping its comments intact -
the file stays the source of truth and is still the nicer way to set per-app
rules and the dictionary. Changes that need a restart raise a banner with a
button rather than applying silently.

## Cleanup providers

The model that cleans up your transcript is a dropdown:

| Provider | Notes |
|---|---|
| **Ollama** (default) | Local, offline, free. Nothing leaves the machine. |
| **Anthropic** | Claude, via the official SDK. |
| **OpenAI** | GPT. |
| **OpenRouter** | One key, hundreds of models — DeepSeek, Qwen, Llama, Gemini, Mistral. |
| **DeepSeek** | DeepSeek directly, if you would rather not go through a router. |
| **Other** | Any OpenAI-compatible endpoint: Groq, Together, Fireworks, vLLM, llama.cpp, LM Studio. Supply the address. |
| **None** | Paste the raw transcript, no cleanup. |

Everything except Ollama and Anthropic speaks the OpenAI protocol, so they
share one implementation and differ only by base URL and which key opens them —
adding a provider is a row in a table, not a new class. Error messages name the
provider you are actually on, because "OpenAI rejected the key" while you are
on OpenRouter sends you to the wrong dashboard.

### Model lists

Fetched from the provider, never hardcoded. A list baked into the source goes
stale within weeks: the IDs written here by hand were already wrong when
checked against the live catalogue.

- **OpenRouter** publishes its catalogue without authentication, so the
  dropdown fills with all ~370 usable models *before* you paste a key.
- **Anthropic** and **OpenAI** are listed through their SDKs once a key is set.
  The Anthropic page object auto-paginates; it is asked for a large page purely
  to save round trips.

The raw catalogue is not the same as the list of models that can clean up a
transcript, so it is filtered:

- `:batch` variants are dropped everywhere — they only serve a batch endpoint
  and reject a synchronous request.
- For OpenAI, embeddings, speech, image, moderation and realtime models are
  dropped by name. Excluding known families beats allow-listing, since new chat
  models appear constantly and an allow-list would hide them. The filter is
  scoped to OpenAI, so a router that happens to route `openai/gpt-4o-audio`
  keeps it.

The list is sorted, which groups a router's models by vendor, and the dropdown
is searchable.

Cleanup is a short rewrite that the whole dictation waits on, so the hosted
backends are configured for latency rather than depth - low effort, no
streaming, small output cap.

**API keys go in the GNOME keyring, never in `config.toml`.** That file ends up
in backups and dotfile repos. An environment variable (`ANTHROPIC_API_KEY`,
`OPENAI_API_KEY`) still wins when set, and the window says which source is in
use so it is never ambiguous which of two keys is live.

Whatever the provider, a failure falls back to pasting the raw transcript -
switching to a cloud model must not mean losing a dictation when the network
drops.

## Languages

English and Dutch. The recogniser (Parakeet TDT v3) detects which you are
speaking on its own, so there is nothing to switch when you change language
mid-session, and it costs nothing in speed over the English-only v2.

`[cleanup] output_language` decides what comes out:

| value | behaviour |
|---|---|
| `same` | whatever you spoke, cleaned up in that language (default) |
| `en` | always English, translating your Dutch |
| `nl` | always Dutch, translating your English |

Translating is a third pass, for the same reason intent resolution is a second
one: asked to clean up *and* translate in one call, the model does whichever
the prompt mentions last. A disfluent English sentence requested in Dutch came
back cleaned but still English, every time. Split apart, both work.

Two things that had to be stated explicitly, because the system prompt is
itself English and quietly pulls output towards English:

- **Never translate in `same` mode.** Before this rule, Dutch came back as
  English about half the time.
- **English loanwords do not make a sentence English.** "Kun je kijken naar de
  authenticatie middleware voordat we deployen?" is Dutch, and was the sentence
  that kept getting translated.

Retraction cues are detected in both languages — "nee wacht", "vergeet de...",
"laat maar", "ik bedoelde". `"vergeet niet"` is excluded, since it means the
opposite.

## What the cleanup pass does

Parakeet turns audio into words, then a local LLM decides what you meant by
them. The LLM stage is itself two passes, but the first only runs when the
transcript contains a retraction cue, so ordinary dictation costs one call:

1. **Resolve** (conditional) — delete ideas you abandoned, and nothing else.
2. **Polish** (always) — everything below.

They are separate calls because the model cannot do both at once: with grammar
repair first it treats a later retraction as content to keep; with retraction
first it stops repairing grammar. Measured both orderings — each fixed one case
and broke the other.

The polish pass:

- fixes punctuation, capitalisation and paragraph breaks
- removes disfluencies, stutters and false starts, including a word abandoned
  part-way and restarted
- removes filler uses of "like", "you know", "sort of", "basically" — while
  keeping them where they mean something ("something like this", "I like it")
- **repairs the fragments disfluency removal leaves behind**, so every sentence
  is grammatical, and splits run-on speech into separate sentences
- **rephrases tangled sentences** — words tripping over each other, a clause
  that never lands — into what you were reaching for, in your own vocabulary.
  A sentence that already reads well is left exactly as spoken
- assembles spoken URLs, emails and paths: "W W dot youtube dot com" becomes
  `www.youtube.com`, "ceyhun at gmail dot com" becomes `ceyhun@gmail.com`
- **resolves a change of mind**: when you abandon an idea mid-dictation
  ("actually no, forget that, what I need is..."), the output reads as if you
  had only ever said the final version
- keeps a second, different request when you are adding rather than replacing
- keeps discourse words (yeah, okay, so) that carry tone, and keeps slang and
  swearing — fixing grammar must not turn casual speech into business writing
- never answers, acts on, or executes what you dictated

`scripts/eval-cleanup.py` pins all of that down against the live model - 24
cases - eight taken verbatim from real dictations where the failures actually
showed up, and four in Dutch. They cover each behaviour and, importantly, the ways it can
go wrong:
eating a second request that merely sounded like a correction, deleting a
discourse word, or answering a dictated question.

**The guard.** Asking the model to repair grammar necessarily lets it reword,
and the failure mode of that licence is answering your dictation instead of
transcribing it. Cleanup only ever condenses — fillers go, retractions go,
spoken URLs collapse — so growth is the tell. An output more than 1.5x longer
than the input, or containing a code block, is rejected and the raw transcript
pastes instead.

**Detecting retractions cheaply.** The second pass is gated on a regex, and
that regex was tuned against 44 real dictations. "actually" alone fired 11
times and was a genuine retraction once, so it now only counts next to a
negation ("actually no", "actually wait"). After tightening: 2 fires, both
genuine, no false positives.

**On reasoning.** Qwen3 can think before answering, and it is off by default.
Measured on the corpus it scored identically - 11/11 either way - while taking
3-21s instead of 0.1-0.3s, and on the longest case it reasoned its way to a
worse answer. Stating the rules precisely beat letting the model deliberate.
`[cleanup] think = "auto"` reasons only on long transcripts with a retraction
cue, `"always"` on everything.

## Learning your vocabulary (off by default)

Switched on, Flow stores your dictations locally and periodically mines two
things from them with the same local model:

- **vocabulary** — the project names, tools and jargon a general recogniser
  mangles, so the cleanup pass spells them the way you do
- **style** — one instruction describing how you actually write, fed back in so
  editing does not sand your voice off

Both are fed into the cleanup prompt, so the longer you use it the more it
sounds like you.

```sh
flow learning on          # start learning, and start using what it learns
flow learning off         # stop both; the profile is kept for next time
flow learning status      # what it knows
flow vocab                # see the terms and the style note
flow vocab --forget "X"   # drop a term and never learn it again
flow learn                # re-mine now instead of waiting
flow history              # what is stored
flow history --clear      # delete all of it
```

**While it is off, Flow stores nothing you dictate** — the transcript exists
only long enough to be pasted. Everything is a local SQLite file under
`~/.local/share/flow`; nothing leaves the machine.

Mining runs in the background, never between your key release and the paste,
and its failures are swallowed — learning must never break dictation.

**Two guards, because a learned term is handed to another model as correct
spelling.** A fabricated one becomes a word Flow will insert into text you
never said:

- Every mined term is **checked against your actual history** and dropped
  unless it appears at least twice. This is not optional politeness: the first
  version of the prompt included example terms, and the model returned five of
  them back as "learned" — none of which this user had ever said.
- Mining cannot tell jargon from a mis-transcription you later corrected. It
  learned "Cafe" from a sentence whose next dictation was "I meant KVK, not
  cafe". So `flow vocab --forget` blocks a term permanently, and the last word
  is yours.

## Configuration

`~/.config/flow/config.toml`, created by `flow config`. The parts worth knowing:

- `[cleanup] dictionary` — names and jargon the recogniser mangles. They are
  given to the cleanup model, which fixes the spelling in context.
- `[cleanup.app_rules]` — per-application tone, keyed by WM class. Run
  `flow context` with the target app focused to find its key. For example
  `"org.gnome.Console" = "Output a shell command only, no prose."`
- `[cleanup] style` — `light` (punctuation only), `balanced`, or `tidy`
  (also tightens grammar).
- `[cleanup] resolve_intent` — the change-of-mind rule. The only rule that
  deletes content; set `false` to keep everything you said.
- `[stt] provider` — `auto` prefers CUDA and falls back to CPU.
- `[cleanup] enabled = false` pastes the raw transcript, skipping the model.

## Pinned dependencies, and why

`onnxruntime` is pinned below 1.23. Newer wheels are built against CUDA 13
while this machine has 12.9, and 1.23 added an external-data path check that
rejects HuggingFace's cache layout, where `model.onnx` and `model.onnx.data`
are separate blobs in different directories. Either alone breaks model loading.

cuDNN and cuBLAS come from the `nvidia-*` pip wheels, which install into
`site-packages/nvidia/*/lib` — a directory on no loader search path. PyTorch
dlopens them at import; onnxruntime does not, and silently falls back to CPU.
`flowd/stt.py` preloads them with `RTLD_GLOBAL` before creating the session.

## Known limits

- Only `text/plain` is preserved across the paste. An image or rich-text
  clipboard selection is lost. Wispr Flow behaves the same way.
- Dictating with the Overview open pastes into the Overview search entry — it
  holds a keyboard grab even though the focused window is still your app.
- GNOME extensions break across major GNOME releases; expect a fix-up at 50.
- English and Dutch only, by choice. Parakeet v3 actually covers 25 European
  languages, so a sentence in a third language will be transcribed rather than
  rejected — the cleanup model is simply told to expect English or Dutch.
- Dutch accuracy is **unverified on real speech**. It was developed against
  espeak-ng synthetic Dutch, which mispronounces badly (it renders "werkt" as
  "merkt"), so the transcription errors seen in testing are probably the test
  audio, not the model.
- The cleanup model adds latency proportional to output length. Recognition is
  effectively free at 334x realtime; the model is the slow part.
