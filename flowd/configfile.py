"""Edit config.toml in place, keeping its comments.

tomllib only reads, and a round-trip writer would strip the explanations above
each setting - which for this file is most of its value. Editing the one line
that changed keeps the file readable by hand, which is still the primary way
to configure Flow.
"""

from __future__ import annotations

import re

from .config import CONFIG_PATH, write_default_config


def _format(value) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return str(value)
    if isinstance(value, (list, tuple)):
        return "[" + ", ".join(f'"{v}"' for v in value) + "]"
    return '"' + str(value).replace('"', '\\"') + '"'


def set_value(section: str, key: str, value) -> None:
    """Set one key inside one section, creating either if missing."""
    path = write_default_config()
    text = path.read_text()
    rendered = _format(value)

    header = re.compile(rf"^\[{re.escape(section)}\]\s*$", re.M)
    match = header.search(text)
    if not match:
        text = text.rstrip() + f"\n\n[{section}]\n{key} = {rendered}\n"
        path.write_text(text)
        return

    # Bound the search to this section so an identically named key in another
    # section is not clobbered.
    start = match.end()
    next_section = re.compile(r"^\[", re.M).search(text, start)
    end = next_section.start() if next_section else len(text)
    body = text[start:end]

    line = re.compile(rf"^(\s*{re.escape(key)}\s*=\s*).*$", re.M)
    if line.search(body):
        body = line.sub(lambda m: m.group(1) + rendered, body, count=1)
    else:
        body = body.rstrip("\n") + f"\n{key} = {rendered}\n"

    path.write_text(text[:start] + body + text[end:])


def get_path() -> str:
    return str(CONFIG_PATH)
