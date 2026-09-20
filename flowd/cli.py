"""The `flow` command."""

from __future__ import annotations

import logging
import os
import shutil
import subprocess
import sys

import typer
from rich.console import Console
from rich.table import Table

from . import __version__
from .config import CONFIG_PATH, Config, write_default_config

app = typer.Typer(
    add_completion=False,
    help="Local offline dictation for GNOME Wayland.",
    no_args_is_help=True,
)
console = Console()

SERVICE = "flow.service"
EXTENSION_UUID = "flow@ceyhun.dev"


def _setup_logging(verbose: bool) -> None:
    logging.basicConfig(
        level=logging.DEBUG if verbose else logging.INFO,
        format="%(asctime)s %(levelname)-7s %(name)s: %(message)s",
        datefmt="%H:%M:%S",
    )


def _systemctl(*args: str) -> int:
    return subprocess.run(["systemctl", "--user", *args]).returncode


@app.command()
def run(verbose: bool = typer.Option(False, "--verbose", "-v")) -> None:
    """Run the daemon in the foreground."""
    _setup_logging(verbose)

    from .daemon import Daemon
    from .island import IslandUnavailable

    try:
        raise SystemExit(Daemon(Config.load()).run())
    except IslandUnavailable as exc:
        console.print(f"[red]error:[/] {exc}")
        raise SystemExit(1)


@app.command()
def gui() -> None:
    """Open the settings window."""
    # GTK's default renderer probes Vulkan, and on a hybrid Intel/NVIDIA box
    # Intel's driver prints "FINISHME" warnings to stderr on every launch.
    # They are harmless but they look like errors. The OpenGL renderer draws
    # this window identically without touching Vulkan. Set before importing
    # the GUI, since GTK reads it at initialisation - and only when the user
    # has not chosen a renderer themselves.
    os.environ.setdefault("GSK_RENDERER", "ngl")

    from .gui import main as gui_main

    raise SystemExit(gui_main())


@app.command()
def start() -> None:
    """Start Flow in the background (this is what the app icon does)."""
    code = _systemctl("start", SERVICE)
    if code == 0:
        console.print("[green]Flow is running.[/] Press your dictation hotkey to talk.")
    raise SystemExit(code)


@app.command()
def stop() -> None:
    """Stop Flow."""
    raise SystemExit(_systemctl("stop", SERVICE))


@app.command()
def restart() -> None:
    """Restart Flow."""
    raise SystemExit(_systemctl("restart", SERVICE))


@app.command()
def status() -> None:
    """Show whether Flow is running."""
    raise SystemExit(_systemctl("status", "--no-pager", SERVICE))


@app.command()
def logs(follow: bool = typer.Option(True, "--follow/--no-follow", "-f")) -> None:
    """Show the daemon's log."""
    args = ["--user", "-u", SERVICE, "-n", "200"]
    if follow:
        args.append("-f")
    raise SystemExit(subprocess.run(["journalctl", *args]).returncode)


@app.command()
def dictate(
    seconds: float = typer.Option(5.0, "--seconds", "-s", help="How long to record."),
    verbose: bool = typer.Option(False, "--verbose", "-v"),
) -> None:
    """Record for a fixed time and insert the result - tests the whole path."""
    _setup_logging(verbose)

    from .daemon import Daemon
    from .island import IslandUnavailable

    try:
        daemon = Daemon(Config.load())
    except IslandUnavailable as exc:
        console.print(f"[red]error:[/] {exc}")
        raise SystemExit(1)

    console.print(f"Loading {daemon.config.stt.model} ...")
    daemon.transcriber.load()
    console.print(f"[bold green]Speak now[/] - recording for {seconds:.0f}s")

    text = daemon.dictate_once(seconds)
    console.print(f"\n[bold]{text}[/]" if text else "[yellow]nothing recognised[/]")


@app.command()
def devices() -> None:
    """List input devices."""
    from .audio import list_devices

    table = Table("index", "name", "channels", "default rate")
    for device in list_devices():
        table.add_row(
            str(device["index"]), device["name"],
            str(device["max_input_channels"]), f"{device['default_samplerate']:.0f}",
        )
    console.print(table)


@app.command()
def context() -> None:
    """Print the focused window - use it to find WM classes for app rules."""
    from .island import IslandClient, IslandUnavailable

    try:
        found = IslandClient().focus_context()
    except IslandUnavailable as exc:
        console.print(f"[red]error:[/] {exc}")
        raise SystemExit(1)

    for key, value in found.items():
        console.print(f"{key:>6}: {value}")


@app.command()
def config(edit: bool = typer.Option(False, "--edit", "-e")) -> None:
    """Show or create the config file."""
    path = write_default_config()
    console.print(f"[dim]{path}[/]")
    if edit:
        editor = shutil.which(sys.argv[0] and "editor") or shutil.which("nano") or "vi"
        subprocess.run([editor, str(path)])
    else:
        console.print(path.read_text())


def _set_learning(enabled: bool) -> None:
    """Flip the toggle in config.toml, preserving its comments.

    tomllib only reads, and a full round-trip writer would strip the
    explanation that sits above this setting - which is the part that says
    nothing is recorded while it is off.
    """
    import re

    path = write_default_config()
    text = path.read_text()
    pattern = re.compile(r"(\[learning\][^\[]*?\benabled\s*=\s*)(true|false)", re.S)
    if not pattern.search(text):
        text = text.rstrip() + f"\n\n[learning]\nenabled = {str(enabled).lower()}\n"
    else:
        text = pattern.sub(lambda m: m.group(1) + str(enabled).lower(), text, count=1)
    path.write_text(text)


@app.command()
def learning(
    action: str = typer.Argument("status", help="on | off | status"),
) -> None:
    """Switch vocabulary learning on or off.

    Off by default, and while it is off Flow stores nothing you dictate.
    """
    action = action.lower()
    if action not in {"on", "off", "status"}:
        console.print("[red]error:[/] expected on, off or status")
        raise SystemExit(2)

    if action in {"on", "off"}:
        _set_learning(action == "on")
        console.print(
            f"learning [bold]{action}[/] - "
            + ("Flow will store your dictations locally and learn from them."
               if action == "on" else
               "Flow will stop storing and stop using what it learned.")
        )
        if subprocess.run(["systemctl", "--user", "is-active", "--quiet", SERVICE]).returncode == 0:
            _systemctl("restart", SERVICE)
            console.print("[dim]restarted flow.service[/]")
        return

    cfg = Config.load()
    console.print(f"learning: [bold]{'on' if cfg.learning.enabled else 'off'}[/]")
    if not cfg.learning.enabled:
        console.print("[dim]nothing is being stored. turn on with: flow learning on[/]")
        return

    from .history import History
    from .learning import load_profile

    history = History()
    terms, style = load_profile(history)
    console.print(f"dictations stored: {history.count()}")
    console.print(f"terms learned:     {len(terms)}")
    if style:
        console.print(f"style:             {style}")


@app.command()
def vocab(
    forget: str = typer.Option(
        "", "--forget", help="Drop a term and never learn it again."
    ),
) -> None:
    """Show the vocabulary Flow has picked up from you."""
    from .history import History
    from .learning import VOCAB_KEY, blocked_terms, forget_term, load_profile

    history = History()

    if forget:
        removed = forget_term(history, forget)
        console.print(
            f"forgot [bold]{forget}[/]" if removed
            else f"[yellow]{forget}[/] was not in the vocabulary; blocked anyway"
        )
        console.print("[dim]run 'flow restart' for the daemon to pick it up[/]")
        return
    terms, style = load_profile(history)
    meta = history.profile_meta(VOCAB_KEY)

    if not terms and not style:
        console.print("[yellow]nothing learned yet.[/]")
        cfg = Config.load()
        if not cfg.learning.enabled:
            console.print("[dim]learning is off - turn it on with: flow learning on[/]")
        else:
            console.print(
                f"[dim]needs {cfg.learning.min_dictations} dictations, "
                f"then refreshes every {cfg.learning.refresh_every}. "
                f"force it now with: flow learn[/]"
            )
        return

    if style:
        console.print(f"[bold]style[/]\n  {style}\n")
    if terms:
        console.print(f"[bold]vocabulary[/] ({len(terms)} terms)")
        for term in terms:
            console.print(f"  {term}")
    blocked = blocked_terms(history)
    if blocked:
        console.print(f"\n[dim]blocked: {', '.join(blocked)}[/]")
    if meta:
        console.print(
            f"\n[dim]learned from {meta['samples']} dictations, "
            f"updated {meta['updated_at']}[/]"
        )
    console.print("[dim]drop a wrong one with: flow vocab --forget \"term\"[/]")


@app.command()
def learn() -> None:
    """Re-read your history and update the profile now."""
    cfg = Config.load()
    if not cfg.learning.enabled:
        console.print("[red]learning is off.[/] turn it on with: flow learning on")
        raise SystemExit(1)

    from .history import History
    from .learning import Learner

    history = History()
    if history.count() == 0:
        console.print("[yellow]no dictations stored yet.[/]")
        raise SystemExit(1)

    console.print(f"mining {history.count()} dictations with {cfg.cleanup.model} ...")
    terms, style = Learner(cfg.cleanup.endpoint, cfg.cleanup.model).refresh(
        history, cfg.learning.max_terms
    )
    console.print(f"learned {len(terms)} terms")
    if style:
        console.print(f"style: {style}")
    console.print("[dim]run 'flow restart' for the daemon to pick it up[/]")


@app.command()
def history(
    limit: int = typer.Option(20, "--limit", "-n"),
    clear: bool = typer.Option(False, "--clear", help="Delete everything stored."),
) -> None:
    """Show, or delete, the dictations Flow has stored."""
    from .history import History

    store = History()
    if clear:
        removed = store.clear()
        console.print(f"deleted {removed} stored dictation(s) and the learned profile.")
        return

    rows = store.recent(limit)
    if not rows:
        console.print("[yellow]nothing stored.[/] learning is probably off.")
        return

    table = Table("when", "app", "said", "pasted")
    for row in reversed(rows):
        table.add_row(row["at"][11:19], (row["app"] or "")[:18],
                      row["raw"][:46], row["clean"][:46])
    console.print(table)


@app.command()
def doctor() -> None:
    """Check every moving part and say what is wrong."""
    cfg = Config.load()
    rows: list[tuple[str, bool, str]] = []

    # Shell extension
    try:
        from .island import IslandClient

        island = IslandClient()
        found = island.focus_context()
        rows.append(("Shell extension", True, f"connected (focus: {found.get('app') or 'none'})"))
    except Exception as exc:  # noqa: BLE001
        rows.append(("Shell extension", False, str(exc)[:90]))

    # Microphone
    try:
        from .audio import list_devices

        inputs = list_devices()
        rows.append(("Microphone", bool(inputs), f"{len(inputs)} input device(s)"))
    except Exception as exc:  # noqa: BLE001
        rows.append(("Microphone", False, str(exc)[:90]))

    # ONNX runtime / CUDA
    try:
        import onnxruntime as ort

        providers = ort.get_available_providers()
        cuda = "CUDAExecutionProvider" in providers
        rows.append((
            "GPU (onnxruntime)", cuda,
            "CUDA available" if cuda else "CPU only - still ~36x realtime",
        ))
    except Exception as exc:  # noqa: BLE001
        rows.append(("GPU (onnxruntime)", False, str(exc)[:90]))

    # Cleanup model
    from .cleanup import build_cleaner

    ok, why = build_cleaner(cfg.cleanup).available()
    rows.append((f"Cleanup ({cfg.cleanup.model})", ok, why))

    # systemd unit
    unit = subprocess.run(
        ["systemctl", "--user", "is-active", SERVICE],
        capture_output=True, text=True,
    ).stdout.strip()
    rows.append(("Service", unit == "active", unit or "not installed"))

    if cfg.learning.enabled:
        from .history import History
        from .learning import load_profile

        try:
            store = History()
            terms, _ = load_profile(store)
            rows.append(("Learning", True,
                         f"on - {store.count()} stored, {len(terms)} terms learned"))
        except Exception as exc:  # noqa: BLE001
            rows.append(("Learning", False, str(exc)[:90]))
    else:
        rows.append(("Learning", True, "off - nothing is being stored"))

    table = Table("check", "", "detail")
    for name, good, detail in rows:
        table.add_row(name, "[green]ok[/]" if good else "[red]fail[/]", detail)
    console.print(table)

    if not all(good for _, good, _ in rows):
        console.print("\n[dim]Anything failing above is explained in the README.[/]")


@app.command()
def version() -> None:
    """Print the version."""
    console.print(f"flow {__version__}")


def main() -> None:
    app()


if __name__ == "__main__":
    main()
