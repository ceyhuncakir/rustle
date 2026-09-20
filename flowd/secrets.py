"""API keys, stored in the GNOME keyring rather than the config file.

config.toml is plain text that ends up in backups, dotfile repos and over
anyone's shoulder. Keys live in libsecret instead, unlocked with the login
session like every other credential on the machine. An environment variable
still wins when one is set, so CI and one-off runs work without touching the
keyring.
"""

from __future__ import annotations

import logging
import os

import gi

gi.require_version("Secret", "1")
from gi.repository import Secret  # noqa: E402

log = logging.getLogger(__name__)

# Env var checked before the keyring, per provider.
ENV_VARS = {
    "anthropic": "ANTHROPIC_API_KEY",
    "openai": "OPENAI_API_KEY",
}

SCHEMA = Secret.Schema.new(
    "ai.flow.ApiKey",
    Secret.SchemaFlags.NONE,
    {"provider": Secret.SchemaAttributeType.STRING},
)


def get_key(provider: str) -> str:
    """The key for a provider: environment first, then the keyring."""
    env = ENV_VARS.get(provider)
    if env and os.environ.get(env):
        return os.environ[env].strip()

    try:
        value = Secret.password_lookup_sync(SCHEMA, {"provider": provider}, None)
    except Exception as exc:  # noqa: BLE001 - a locked keyring is not fatal
        log.warning("could not read the keyring: %s", exc)
        return ""
    return (value or "").strip()


def set_key(provider: str, key: str) -> bool:
    try:
        return Secret.password_store_sync(
            SCHEMA, {"provider": provider}, Secret.COLLECTION_DEFAULT,
            f"Flow - {provider} API key", key.strip(), None,
        )
    except Exception as exc:  # noqa: BLE001
        log.warning("could not write to the keyring: %s", exc)
        return False


def clear_key(provider: str) -> bool:
    try:
        return Secret.password_clear_sync(SCHEMA, {"provider": provider}, None)
    except Exception as exc:  # noqa: BLE001
        log.warning("could not clear the keyring entry: %s", exc)
        return False


def key_source(provider: str) -> str:
    """Where the key is coming from - shown in the GUI so it is never a
    mystery which of two possible keys is actually in use."""
    env = ENV_VARS.get(provider)
    if env and os.environ.get(env):
        return f"${env}"
    return "keyring" if get_key(provider) else "not set"
