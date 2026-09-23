# Manual QA checklist

Run on every platform before a release. Each line is pass/fail; note the OS,
version and desktop (for Linux: X11 / GNOME Wayland / KDE Wayland / Hyprland).

## Fresh install

- [ ] Install from the release artefact using only the README.
- [ ] First launch opens the setup wizard; every step completes.
- [ ] Wizard "Test paste" lands the marker in the wizard's own field and the
      previous clipboard text comes back.
- [ ] Model download shows progress, survives a cancel and resume, and the
      self-transcription test returns the expected text.
- [ ] After the wizard, the tray icon is present and dictation is on.

## Dictation

- [ ] Hold the hotkey, speak, release: island shows listening → thinking →
      inserting, text lands in the focused app, island hides.
- [ ] Tap the hotkey (under 350 ms), speak, tap again: same result.
- [ ] Cancel shortcut while recording: island hides, nothing pasted.
- [ ] Release after less than 0.35 s: "Too short" error, hides after 2.5 s.
- [ ] Hold in silence: "No speech detected".
- [ ] Paste into: a plain text editor, VS Code, a browser text field, a
      terminal.
- [ ] Clipboard restore: copy text first, dictate, paste again → original text.
- [ ] Clipboard restore with an image copied: dictation pastes; image is lost
      (documented limitation) and no crash.
- [ ] The island never takes keyboard focus from the target app.
- [ ] The island never receives clicks (click-through).
- [ ] Island position: bottom-centre of the primary monitor, above a bottom
      dock or panel; correct on a second monitor set as primary.
- [ ] Long transcript: island widens up to 55 % of the screen, label
      ellipsised.
- [ ] Journal / log shows `inserted in X s (spoke Y s)` per dictation.

## Graphics card

- [ ] `flow gpu` names every card, says which one recognition uses, and the
      log line `loaded ... on webgpu` (or `on cpu`) agrees with it.
- [ ] A discrete AMD, Intel Arc or NVIDIA card: recognition runs on it and
      the transcript matches a CPU run of the same take.
- [ ] Only an integrated GPU: `auto` stays on the CPU; "GPU only" in the
      settings moves it to the GPU after a restart.
- [ ] No Vulkan driver (Linux): `flow gpu` names the card and the missing
      driver; dictation still works on the CPU.
- [ ] Apple Silicon: recognition runs on the GPU from the installed app
      (Dawn is found in Contents/Frameworks).
- [ ] Windows: recognition runs on the GPU from the installed app
      (webgpu_dawn.dll, dxcompiler.dll and dxil.dll beside Flow.exe).
- [ ] Quit from the tray, `systemctl --user stop flow` and Ctrl-C each end
      Flow within a second or two with exit code 0; the log shows `unloaded
      <model> from Ollama`, `unloaded <recogniser>` and `released ONNX
      Runtime`; afterwards `ollama ps` is empty and `nvidia-smi` lists no
      flow process.

## Settings

- [ ] Every control writes to `config.toml` and the file's comments survive.
- [ ] Hotkey recorder: rebinding takes effect without restart; a conflict is
      reported.
- [ ] Changing the recognition model shows the restart banner; Restart works.
- [ ] Provider switch: model list refreshes; API key saved to the keyring;
      key from an environment variable is reported as such.
- [ ] Learning on: after 15 dictations vocabulary appears; "Forget all"
      clears it.
- [ ] "Check setup" is green on a working install and names the broken part
      otherwise.
- [ ] "Copy diagnostics" puts a useful report on the clipboard.

## Platform specifics

### Windows
- [ ] Hotkey release detected within ~50 ms (polled).
- [ ] Island stays above a fullscreen browser window.
- [ ] Launch at login works; app starts minimised to the tray.

### macOS
- [ ] Accessibility prompt appears on first paste; paste works after grant.
- [ ] Screen Recording declined: dictation still works, window titles empty.
- [ ] Terminal with "Secure Keyboard Entry" on: hotkey still works.
- [ ] Non-US keyboard layout (e.g. Dvorak, AZERTY): paste chord still works.
- [ ] Notarised DMG opens with no Gatekeeper warning.
- [ ] Island is a non-activating panel: the app under it keeps focus.

### Linux GNOME Wayland
- [ ] Extension enabled after logout; `flow doctor` reports its version.
- [ ] `flow --headless` under the user service works without any window.
- [ ] Overview open while dictating: documented quirk, no crash.

### Linux KDE / Hyprland
- [ ] Paste helper detected (dotool or ydotool); missing helper is reported.
- [ ] Layer-shell island does not steal focus; paste-back self-test passes.
- [ ] Overlay mode "off" and "window" both behave as described.

### Linux X11
- [ ] Hotkey grabbed; paste via XTEST works in GTK and Qt apps.

## Update

- [ ] "Check for updates" finds the next version; update installs and the
      app restarts on the new version with settings intact.
