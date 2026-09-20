"""Backend selection, and the config writer the GUI depends on.

The risk here is quiet misconfiguration: a provider switch that leaves the
previous provider's model behind, or a config write that eats the comments
that make the file worth editing by hand.
"""

from __future__ import annotations

import pytest

from flowd.backends import (
    PROVIDERS,
    AnthropicBackend,
    BackendError,
    OllamaBackend,
    OpenAIBackend,
    build_backend,
)
from flowd.config import CleanupConfig


def test_every_provider_is_described():
    for key, spec in PROVIDERS.items():
        assert spec.key == key
        assert spec.label
        # "none" has no model, and "custom" cannot have one - the user brings
        # their own endpoint. Everything else must offer its default.
        if spec.default_model:
            assert spec.default_model in spec.suggested_models, key


def test_custom_provider_has_no_preset_model_or_url():
    """It is the escape hatch: the user supplies both."""
    spec = PROVIDERS["custom"]
    assert spec.needs_api_key
    assert not spec.default_model
    assert spec.base_url is None


def test_routers_carry_a_base_url():
    for key in ("openrouter", "deepseek"):
        assert PROVIDERS[key].base_url.startswith("https://"), key
        assert PROVIDERS[key].needs_api_key


def test_openai_uses_the_sdk_default_address():
    assert PROVIDERS["openai"].base_url is None


def test_env_vars_match_the_provider_catalogue():
    """secrets duplicates this mapping to avoid an import cycle, so drift is
    caught here rather than by a key silently not being found."""
    from flowd import secrets

    for key, spec in PROVIDERS.items():
        if spec.needs_api_key:
            assert secrets.ENV_VARS.get(key) == spec.env_var, key
        else:
            assert key not in secrets.ENV_VARS, key


def test_hosted_providers_need_a_key_and_local_ones_do_not():
    assert PROVIDERS["anthropic"].needs_api_key
    assert PROVIDERS["openai"].needs_api_key
    assert not PROVIDERS["ollama"].needs_api_key
    assert not PROVIDERS["none"].needs_api_key


@pytest.mark.parametrize(
    "backend,expected",
    [("ollama", OllamaBackend), ("anthropic", AnthropicBackend),
     ("openai", OpenAIBackend), ("openrouter", OpenAIBackend),
     ("deepseek", OpenAIBackend), ("custom", OpenAIBackend),
     ("none", OllamaBackend)],
)
def test_build_backend_picks_the_right_class(backend, expected):
    cfg = CleanupConfig(backend=backend, model="x")
    assert isinstance(build_backend(cfg), expected)


@pytest.mark.parametrize(
    "backend,url",
    [("openrouter", "https://openrouter.ai/api/v1"),
     ("deepseek", "https://api.deepseek.com"),
     ("openai", None)],
)
def test_named_providers_get_their_own_address(backend, url):
    assert build_backend(CleanupConfig(backend=backend, model="x")).base_url == url


def test_custom_provider_uses_the_configured_address():
    cfg = CleanupConfig(backend="custom", model="m",
                        base_url="http://127.0.0.1:8000/v1")
    assert build_backend(cfg).base_url == "http://127.0.0.1:8000/v1"


def test_custom_provider_without_an_address_says_so(monkeypatch):
    monkeypatch.setattr("flowd.secrets.get_key", lambda provider: "sk-test")
    backend = build_backend(CleanupConfig(backend="custom", model="m"))
    ok, why = backend.available()
    assert not ok and "base URL" in why
    with pytest.raises(BackendError, match="base URL"):
        backend.complete("s", "p", 5.0)


def test_hosted_backends_fail_clearly_without_a_key(monkeypatch):
    # A missing key must say so, not surface as a timeout or a stack trace.
    monkeypatch.setattr("flowd.secrets.get_key", lambda provider: "")

    for backend in (
        AnthropicBackend("claude-opus-5"),
        OpenAIBackend("gpt-5", "openai"),
        OpenAIBackend("deepseek/deepseek-chat", "openrouter"),
        OpenAIBackend("deepseek-chat", "deepseek"),
    ):
        ok, why = backend.available()
        assert not ok and "key" in why.lower()
        with pytest.raises(BackendError, match="API key"):
            backend.complete("sys", "prompt", 5.0)


def test_unreachable_ollama_reports_unavailable():
    ok, why = OllamaBackend("m", "http://127.0.0.1:1").available()
    assert not ok and "unreachable" in why


def test_hosted_backends_are_always_warm():
    assert AnthropicBackend("claude-opus-5").warm_up() == 0.0
    assert OpenAIBackend("gpt-5", "openai").warm_up() == 0.0
    assert OpenAIBackend("x", "openrouter").warm_up() == 0.0


def test_error_messages_name_the_provider(monkeypatch):
    """"OpenAI rejected the key" when you are on OpenRouter sends you to the
    wrong dashboard."""
    monkeypatch.setattr("flowd.secrets.get_key", lambda provider: "")
    with pytest.raises(BackendError, match="OpenRouter"):
        OpenAIBackend("x", "openrouter").complete("s", "p", 5.0)
    with pytest.raises(BackendError, match="DeepSeek"):
        OpenAIBackend("x", "deepseek").complete("s", "p", 5.0)


def test_cleaner_failure_falls_back_to_the_raw_transcript(monkeypatch):
    """Switching to a cloud provider must not risk losing a dictation when the
    network is down."""
    from flowd.cleanup import Cleaner

    class Broken:
        def complete(self, *a, **k):
            raise BackendError("no network")

        def available(self):
            return False, "no network"

        def warm_up(self):
            return 0.0

        def unload(self):
            pass

    cleaner = Cleaner(backend=Broken())
    assert cleaner.clean("um hello there") == "um hello there"


# -- the config writer ------------------------------------------------------


@pytest.fixture
def conf(tmp_path, monkeypatch):
    path = tmp_path / "config.toml"
    monkeypatch.setattr("flowd.configfile.write_default_config", lambda: path)
    return path


def test_writes_into_the_right_section(conf):
    from flowd.configfile import set_value

    conf.write_text("[audio]\nmodel = \"mic\"\n\n[stt]\nmodel = \"old\"\n")
    set_value("stt", "model", "new")
    text = conf.read_text()
    assert 'model = "new"' in text
    # The identically named key in [audio] must be untouched.
    assert 'model = "mic"' in text


def test_preserves_comments(conf):
    from flowd.configfile import set_value

    conf.write_text("# explains the setting\n[stt]\n# and this one\nmodel = \"old\"\n")
    set_value("stt", "model", "new")
    text = conf.read_text()
    assert "# explains the setting" in text
    assert "# and this one" in text


def test_adds_a_missing_key_and_section(conf):
    from flowd.configfile import set_value

    conf.write_text("[stt]\nmodel = \"x\"\n")
    set_value("stt", "provider", "cuda")
    set_value("learning", "enabled", True)
    text = conf.read_text()
    assert 'provider = "cuda"' in text
    assert "[learning]" in text and "enabled = true" in text


@pytest.mark.parametrize(
    "value,rendered",
    [(True, "true"), (False, "false"), (12, "12"), (1.5, "1.5"),
     ("hi", '"hi"'), (["a", "b"], '["a", "b"]')],
)
def test_renders_toml_types(conf, value, rendered):
    from flowd.configfile import set_value

    conf.write_text("[x]\nk = 0\n")
    set_value("x", "k", value)
    assert f"k = {rendered}" in conf.read_text()


def test_round_trips_through_the_real_config_loader(conf):
    import tomllib

    from flowd.configfile import set_value

    conf.write_text("[cleanup]\nbackend = \"ollama\"\nmodel = \"qwen3:14b\"\n")
    set_value("cleanup", "backend", "anthropic")
    set_value("cleanup", "model", "claude-opus-5")

    parsed = tomllib.loads(conf.read_text())
    assert parsed["cleanup"] == {"backend": "anthropic", "model": "claude-opus-5"}
