"""Flow's settings window.

GTK4 and libadwaita, so it looks like the rest of the desktop rather than a
web page in a box. Every control writes straight to config.toml - the file
stays the source of truth and remains editable by hand.
"""

from __future__ import annotations

import subprocess
import threading

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Adw", "1")
from gi.repository import Adw, Gio, GLib, Gtk  # noqa: E402

from . import secrets
from .backends import PROVIDERS, OllamaBackend
from .config import Config
from .configfile import get_path, set_value
from .stt import MODELS as STT_MODELS
from .stt import is_downloaded

SERVICE = "flow.service"
EXTENSION_UUID = "flow@ceyhun.dev"

STYLES = [
    ("light", "Light", "Punctuation only, wording untouched"),
    ("balanced", "Balanced", "Also fixes grammar and removes fillers"),
    ("tidy", "Tidy", "Also tightens loose phrasing"),
]

LANGUAGES = [
    ("same", "Same as spoken", "Dutch stays Dutch, English stays English"),
    ("en", "Always English", "Translates your Dutch"),
    ("nl", "Always Dutch", "Translates your English"),
]

COMPUTE = [
    ("auto", "Automatic", "Use the GPU when it is available"),
    ("cuda", "GPU only", "Fail rather than fall back to the CPU"),
    ("cpu", "CPU only", "Slower, but leaves the GPU free"),
]


def _systemctl(*args: str) -> int:
    return subprocess.run(["systemctl", "--user", *args],
                          capture_output=True).returncode


def _service_active() -> bool:
    return _systemctl("is-active", "--quiet", SERVICE) == 0


def _hotkey() -> str:
    try:
        out = subprocess.run(
            ["gsettings", "--schemadir",
             str(GLib.get_home_dir()) + f"/.local/share/gnome-shell/extensions/{EXTENSION_UUID}/schemas",
             "get", "org.gnome.shell.extensions.flow", "toggle-dictation"],
            capture_output=True, text=True, timeout=3,
        ).stdout.strip()
        return out.strip("[]'\"").replace("'", "") or "not set"
    except Exception:  # noqa: BLE001
        return "unknown"


class Row:
    """Small helpers that keep the window code readable."""

    @staticmethod
    def combo(title: str, subtitle: str, labels: list[str], active: int, on_change):
        row = Adw.ComboRow(title=title, subtitle=subtitle)
        row.set_model(Gtk.StringList.new(labels))
        row.set_selected(max(0, active))
        row.connect("notify::selected", on_change)
        return row


class FlowWindow(Adw.ApplicationWindow):
    def __init__(self, app: Adw.Application) -> None:
        super().__init__(application=app, title="Flow", default_width=720,
                         default_height=820)
        self.cfg = Config.load()
        self._loading = True

        self.toasts = Adw.ToastOverlay()
        self.banner = Adw.Banner(title="Restart Flow to apply your changes",
                                 button_label="Restart", revealed=False)
        self.banner.connect("button-clicked", self._on_restart)

        page = Adw.PreferencesPage()
        page.add(self._group_status())
        page.add(self._group_voice())
        page.add(self._group_cleanup())
        page.add(self._group_learning())
        page.add(self._group_about())

        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
        box.append(self.banner)
        box.append(page)
        page.set_vexpand(True)

        view = Adw.ToolbarView()
        view.add_top_bar(Adw.HeaderBar())
        view.set_content(box)
        self.toasts.set_child(view)
        self.set_content(self.toasts)

        self._loading = False
        GLib.timeout_add_seconds(3, self._poll_service)

    # -- status --------------------------------------------------------------

    def _group_status(self) -> Adw.PreferencesGroup:
        group = Adw.PreferencesGroup(title="Status")

        self.run_row = Adw.SwitchRow(
            title="Dictation", subtitle=self._status_subtitle()
        )
        self.run_row.set_active(_service_active())
        self.run_row.connect("notify::active", self._on_toggle_service)
        group.add(self.run_row)

        check = Adw.ActionRow(
            title="Check setup",
            subtitle="Verify the extension, microphone, GPU and model",
        )
        button = Gtk.Button(label="Run", valign=Gtk.Align.CENTER)
        button.connect("clicked", self._on_doctor)
        check.add_suffix(button)
        check.set_activatable_widget(button)
        group.add(check)
        return group

    def _status_subtitle(self) -> str:
        if not _service_active():
            return "Stopped"
        # Subtitles are Pango markup, and a shortcut like "<Super>d" parses as
        # an unclosed tag - which silently blanks the entire subtitle.
        return f"Running - hold {GLib.markup_escape_text(_hotkey())} and talk"

    def _poll_service(self) -> bool:
        active = _service_active()
        if active != self.run_row.get_active():
            self._loading = True
            self.run_row.set_active(active)
            self._loading = False
        self.run_row.set_subtitle(self._status_subtitle())
        return GLib.SOURCE_CONTINUE

    def _on_toggle_service(self, row, _param) -> None:
        if self._loading:
            return
        want = row.get_active()
        # Starting loads both models, which takes a few seconds; do it off the
        # UI thread so the window does not freeze.
        threading.Thread(
            target=lambda: _systemctl("start" if want else "stop", SERVICE),
            daemon=True,
        ).start()
        self._toast("Starting Flow…" if want else "Flow stopped")

    def _on_doctor(self, _button) -> None:
        def run():
            out = subprocess.run(
                ["flow", "doctor"], capture_output=True, text=True
            ).stdout
            bad = out.count("fail")
            GLib.idle_add(
                self._toast,
                "Everything is ready" if not bad else f"{bad} check(s) failing - see 'flow doctor'",
            )

        threading.Thread(target=run, daemon=True).start()

    # -- voice ---------------------------------------------------------------

    def _group_voice(self) -> Adw.PreferencesGroup:
        group = Adw.PreferencesGroup(
            title="Voice", description="How your speech becomes text"
        )

        self.stt_keys = list(STT_MODELS)
        labels = []
        for key in self.stt_keys:
            name, _ = STT_MODELS[key]
            labels.append(name if is_downloaded(key) else f"{name}  (downloads on first use)")
        active = self.stt_keys.index(self.cfg.stt.model) if self.cfg.stt.model in self.stt_keys else 0

        self.stt_row = Row.combo(
            "Recognition model", STT_MODELS[self.stt_keys[active]][1],
            labels, active, self._on_stt_model,
        )
        group.add(self.stt_row)

        keys = [c[0] for c in COMPUTE]
        idx = keys.index(self.cfg.stt.provider) if self.cfg.stt.provider in keys else 0
        group.add(Row.combo("Runs on", COMPUTE[idx][2], [c[1] for c in COMPUTE],
                            idx, self._on_compute))

        self.device_row = Adw.EntryRow(title="Microphone")
        self.device_row.set_text(self.cfg.audio.device)
        self.device_row.set_show_apply_button(True)
        self.device_row.connect("apply", self._on_device)
        group.add(self.device_row)
        return group

    def _on_stt_model(self, row, _p) -> None:
        if self._loading:
            return
        key = self.stt_keys[row.get_selected()]
        row.set_subtitle(STT_MODELS[key][1])
        set_value("stt", "model", key)
        if not is_downloaded(key):
            self._toast("Model will download the next time Flow starts")
        self._needs_restart()

    def _on_compute(self, row, _p) -> None:
        if self._loading:
            return
        choice = COMPUTE[row.get_selected()]
        row.set_subtitle(choice[2])
        set_value("stt", "provider", choice[0])
        self._needs_restart()

    def _on_device(self, row) -> None:
        set_value("audio", "device", row.get_text().strip())
        self._needs_restart()

    # -- cleanup -------------------------------------------------------------

    def _group_cleanup(self) -> Adw.PreferencesGroup:
        group = Adw.PreferencesGroup(
            title="Cleanup",
            description="The model that turns the transcript into what you meant",
        )

        self.provider_keys = list(PROVIDERS)
        idx = (self.provider_keys.index(self.cfg.cleanup.backend)
               if self.cfg.cleanup.backend in self.provider_keys else 0)
        self.provider_row = Row.combo(
            "Provider", "", [PROVIDERS[k].label for k in self.provider_keys],
            idx, self._on_provider,
        )
        group.add(self.provider_row)

        self.url_row = Adw.EntryRow(title="API address")
        self.url_row.set_text(self.cfg.cleanup.base_url)
        self.url_row.set_show_apply_button(True)
        self.url_row.connect("apply", self._on_base_url)
        group.add(self.url_row)

        self.model_row = Adw.ComboRow(title="Model")
        self.model_row.connect("notify::selected", self._on_cleanup_model)
        group.add(self.model_row)

        self.key_row = Adw.PasswordEntryRow(title="API key")
        self.key_row.set_show_apply_button(True)
        self.key_row.connect("apply", self._on_api_key)
        group.add(self.key_row)

        keys = [s[0] for s in STYLES]
        sidx = keys.index(self.cfg.cleanup.style) if self.cfg.cleanup.style in keys else 1
        self.style_row = Row.combo("Editing", STYLES[sidx][2],
                                   [s[1] for s in STYLES], sidx, self._on_style)
        group.add(self.style_row)

        lkeys = [l[0] for l in LANGUAGES]
        lidx = (lkeys.index(self.cfg.cleanup.output_language)
                if self.cfg.cleanup.output_language in lkeys else 0)
        self.lang_row = Row.combo("Output language", LANGUAGES[lidx][2],
                                  [l[1] for l in LANGUAGES], lidx, self._on_language)
        group.add(self.lang_row)

        self._refresh_provider(self.provider_keys[idx])
        return group

    def _refresh_provider(self, provider: str) -> None:
        """Show the fields this provider needs, and its models."""
        spec = PROVIDERS[provider]
        self.provider_row.set_subtitle(spec.note)
        self.key_row.set_visible(spec.needs_api_key)
        self.url_row.set_visible(provider == "custom")
        self.model_row.set_visible(provider != "none")

        if spec.needs_api_key:
            self.key_row.set_title(f"{spec.label} API key")
            source = secrets.key_source(provider)
            if source.startswith("$"):
                self.key_row.set_visible(False)
                self.provider_row.set_subtitle(f"Using the key from {source}")
            else:
                was, self._loading = self._loading, True
                self.key_row.set_text(secrets.get_key(provider))
                self._loading = was

        if provider == "none":
            return

        # Show something immediately, then replace it with the provider's own
        # list once it answers - OpenRouter alone offers hundreds, and no
        # hardcoded list stays right.
        self._set_models(list(spec.suggested_models), spec)
        if provider == "ollama" or secrets.get_key(provider):
            threading.Thread(target=self._fetch_models, args=(provider,),
                             daemon=True).start()

    def _set_models(self, choices: list[str], spec) -> None:
        current = self.cfg.cleanup.model
        if current and current not in choices:
            choices = [current, *choices]
        if not choices:
            choices = [spec.default_model or ""]

        self.model_keys = choices
        was, self._loading = self._loading, True
        self.model_row.set_model(Gtk.StringList.new(choices))
        if current in choices:
            self.model_row.set_selected(choices.index(current))
        self._loading = was

    def _fetch_models(self, provider: str) -> None:
        """Ask the provider what it actually offers. Off the UI thread: this
        is a network call for everything except Ollama."""
        from .backends import build_backend

        cfg = Config.load().cleanup
        cfg.backend = provider
        try:
            found = build_backend(cfg).installed_models()
        except Exception:  # noqa: BLE001 - a provider that will not answer is
            found = []      # not an error worth interrupting the user for

        if not found or provider != self.provider_keys[self.provider_row.get_selected()]:
            return
        GLib.idle_add(self._apply_fetched_models, found, provider)

    def _apply_fetched_models(self, found: list[str], provider: str) -> bool:
        self._set_models(found, PROVIDERS[provider])
        self.model_row.set_subtitle(
            f"{len(found)} available"
            + (" locally" if provider == "ollama" else " from this provider")
        )
        return GLib.SOURCE_REMOVE

    def _on_provider(self, row, _p) -> None:
        if self._loading:
            return
        provider = self.provider_keys[row.get_selected()]
        set_value("cleanup", "backend", provider)

        spec = PROVIDERS[provider]
        if spec.default_model and self.cfg.cleanup.model not in spec.suggested_models:
            set_value("cleanup", "model", spec.default_model)
            self.cfg.cleanup.model = spec.default_model

        self.cfg.cleanup.backend = provider
        self._refresh_provider(provider)
        self._needs_restart()

    def _on_cleanup_model(self, row, _p) -> None:
        if self._loading:
            return
        model = self.model_keys[row.get_selected()]
        set_value("cleanup", "model", model)
        self.cfg.cleanup.model = model
        self._needs_restart()

    def _on_base_url(self, row) -> None:
        set_value("cleanup", "base_url", row.get_text().strip())
        self.cfg.cleanup.base_url = row.get_text().strip()
        self._refresh_provider("custom")
        self._needs_restart()

    def _on_api_key(self, row) -> None:
        provider = self.provider_keys[self.provider_row.get_selected()]
        key = row.get_text().strip()
        if not key:
            secrets.clear_key(provider)
            self._toast("API key removed")
            return
        # Keys go to the GNOME keyring, never into config.toml.
        if secrets.set_key(provider, key):
            self._toast("Saved to the keyring")
            threading.Thread(target=self._fetch_models, args=(provider,),
                             daemon=True).start()
        else:
            self._toast("Could not write to the keyring")
        self._needs_restart()

    def _on_style(self, row, _p) -> None:
        if self._loading:
            return
        choice = STYLES[row.get_selected()]
        row.set_subtitle(choice[2])
        set_value("cleanup", "style", choice[0])
        self._needs_restart()

    def _on_language(self, row, _p) -> None:
        if self._loading:
            return
        choice = LANGUAGES[row.get_selected()]
        row.set_subtitle(choice[2])
        set_value("cleanup", "output_language", choice[0])
        self._needs_restart()

    # -- learning ------------------------------------------------------------

    def _group_learning(self) -> Adw.PreferencesGroup:
        group = Adw.PreferencesGroup(
            title="Learning",
            description="Off by default. While off, nothing you dictate is stored.",
        )

        self.learn_row = Adw.SwitchRow(
            title="Learn my vocabulary",
            subtitle="Picks up your jargon and how you write, and uses both",
        )
        self.learn_row.set_active(self.cfg.learning.enabled)
        self.learn_row.connect("notify::active", self._on_learning)
        group.add(self.learn_row)

        self.vocab_row = Adw.ActionRow(title="What it has learned")
        self._refresh_vocab()
        button = Gtk.Button(label="Forget all", valign=Gtk.Align.CENTER)
        button.add_css_class("destructive-action")
        button.connect("clicked", self._on_forget_all)
        self.vocab_row.add_suffix(button)
        group.add(self.vocab_row)
        return group

    def _refresh_vocab(self) -> None:
        try:
            from .history import History
            from .learning import load_profile

            store = History()
            terms, _ = load_profile(store)
            count = store.count()
        except Exception:  # noqa: BLE001
            terms, count = [], 0

        self.vocab_row.set_subtitle(
            f"{len(terms)} terms from {count} dictations"
            + (f" - {', '.join(terms[:4])}…" if terms else "")
            if count else "Nothing stored yet"
        )

    def _on_learning(self, row, _p) -> None:
        if self._loading:
            return
        set_value("learning", "enabled", row.get_active())
        self._refresh_vocab()
        self._needs_restart()

    def _on_forget_all(self, _button) -> None:
        try:
            from .history import History

            removed = History().clear()
            self._toast(f"Deleted {removed} stored dictation(s)")
        except Exception as exc:  # noqa: BLE001
            self._toast(f"Could not clear: {exc}")
        self._refresh_vocab()

    # -- about ---------------------------------------------------------------

    def _group_about(self) -> Adw.PreferencesGroup:
        group = Adw.PreferencesGroup()
        row = Adw.ActionRow(title="Configuration file", subtitle=get_path())
        button = Gtk.Button(label="Open", valign=Gtk.Align.CENTER)
        button.connect(
            "clicked",
            lambda _b: Gio.AppInfo.launch_default_for_uri(f"file://{get_path()}", None),
        )
        row.add_suffix(button)
        group.add(row)
        return group

    # -- shared --------------------------------------------------------------

    def _toast(self, text: str) -> bool:
        self.toasts.add_toast(Adw.Toast(title=text, timeout=3))
        return GLib.SOURCE_REMOVE

    def _needs_restart(self) -> None:
        if _service_active():
            self.banner.set_revealed(True)

    def _on_restart(self, _banner) -> None:
        self.banner.set_revealed(False)
        threading.Thread(target=lambda: _systemctl("restart", SERVICE),
                         daemon=True).start()
        self._toast("Restarting Flow…")


class FlowApp(Adw.Application):
    def __init__(self) -> None:
        super().__init__(application_id="ai.flow.Settings",
                         flags=Gio.ApplicationFlags.DEFAULT_FLAGS)

    def do_activate(self) -> None:
        window = self.props.active_window or FlowWindow(self)
        window.present()


def main() -> int:
    return FlowApp().run(None)
