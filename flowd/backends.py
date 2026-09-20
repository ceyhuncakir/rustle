"""Where the cleanup model actually runs.

Three interchangeable backends behind one interface, so the choice between a
local model and a hosted one is a dropdown rather than a rewrite:

  ollama    - local, offline, free, nothing leaves the machine (the default)
  anthropic - Claude, via the official SDK
  openai    - GPT, via the official SDK

Cleanup is a short, latency-sensitive rewrite, so the hosted backends are
configured for speed rather than depth: low effort, no streaming, small output
cap. The whole dictation waits on this call.
"""

from __future__ import annotations

import logging
import time
from dataclasses import dataclass

import httpx

from . import secrets

log = logging.getLogger(__name__)

# Cleanup output is bounded by what the speaker said; this is generous.
MAX_TOKENS = 4096


@dataclass
class Provider:
    key: str
    label: str
    needs_api_key: bool
    default_model: str
    # Offered in the GUI before a key is set. Once there is one, the live
    # /models endpoint replaces this, so it only has to be a starting point.
    suggested_models: tuple[str, ...]
    # Set for anything speaking the OpenAI protocol at a non-OpenAI address.
    base_url: str | None = None
    # Env var checked before the keyring.
    env_var: str | None = None
    note: str = ""
    # Catalogue readable without authentication, when the provider has one.
    public_models_url: str | None = None


PROVIDERS = {
    "ollama": Provider(
        "ollama", "Ollama (local)", False, "qwen3:14b",
        ("qwen3:14b", "qwen3:8b", "qwen3:4b", "llama3.1:8b", "mistral-nemo:12b",
         "gemma3:12b", "phi4:14b"),
        note="Runs on this machine. Nothing leaves it.",
    ),
    "anthropic": Provider(
        "anthropic", "Anthropic Claude", True, "claude-opus-5",
        ("claude-opus-5", "claude-sonnet-5", "claude-haiku-4-5"),
        env_var="ANTHROPIC_API_KEY",
    ),
    "openai": Provider(
        "openai", "OpenAI", True, "gpt-5",
        ("gpt-5", "gpt-5-mini", "gpt-4.1", "gpt-4.1-mini"),
        env_var="OPENAI_API_KEY",
    ),
    "openrouter": Provider(
        # Model IDs here go stale fast - OpenRouter publishes its catalogue
        # without authentication, so the real list is always fetched and this
        # is only what shows if the network is down.
        "openrouter", "OpenRouter", True, "deepseek/deepseek-v4.1-flash",
        ("deepseek/deepseek-v4.1-flash",),
        base_url="https://openrouter.ai/api/v1",
        env_var="OPENROUTER_API_KEY",
        public_models_url="https://openrouter.ai/api/v1/models",
        note="Hundreds of models from one key. The full list loads below.",
    ),
    "deepseek": Provider(
        "deepseek", "DeepSeek", True, "deepseek-chat",
        ("deepseek-chat", "deepseek-reasoner"),
        base_url="https://api.deepseek.com",
        env_var="DEEPSEEK_API_KEY",
    ),
    "custom": Provider(
        "custom", "Other (OpenAI-compatible)", True, "",
        (),
        env_var="FLOW_API_KEY",
        note="Any OpenAI-compatible endpoint: Groq, Together, Fireworks, "
             "vLLM, llama.cpp, LM Studio.",
    ),
    "none": Provider("none", "No cleanup (raw transcript)", False, "", (),
                     note="Paste the transcript exactly as recognised."),
}


class BackendError(RuntimeError):
    pass


# Substrings that mark a model as unable to answer a chat completion. OpenAI's
# /v1/models returns its whole catalogue - embeddings, speech, images and
# moderation included - and offering those as a cleanup model is a guaranteed
# runtime error. Excluding known families beats allow-listing, because new
# chat models appear constantly and an allow-list would hide them.
NON_CHAT_MARKERS = (
    "embedding", "tts", "whisper", "transcribe", "dall-e", "moderation",
    "image", "audio", "realtime", "sora", "babbage", "davinci", "search",
    "computer-use", "codex-mini",
)


def usable_chat_models(ids: list[str], provider: str) -> list[str]:
    """Drop anything that cannot serve a normal chat completion."""
    kept = []
    for model in ids:
        # ":batch" variants exist only for a provider's batch endpoint and
        # reject a synchronous request.
        if model.endswith(":batch"):
            continue
        if provider == "openai" and any(m in model.lower() for m in NON_CHAT_MARKERS):
            continue
        kept.append(model)
    return sorted(kept)


class OllamaBackend:
    """Local models through Ollama's HTTP API."""

    key = "ollama"

    def __init__(self, model: str, endpoint: str = "http://localhost:11434",
                 keep_alive: str = "1h") -> None:
        self.model = model
        self.endpoint = endpoint.rstrip("/")
        self.keep_alive = keep_alive

    def complete(self, system: str, prompt: str, timeout: float,
                 thinking: bool = False) -> str:
        reply = httpx.post(
            f"{self.endpoint}/api/generate",
            json={
                "model": self.model, "system": system, "prompt": prompt,
                "stream": False, "think": thinking, "keep_alive": self.keep_alive,
                "options": {"temperature": 0.1},
            },
            timeout=timeout,
        )
        reply.raise_for_status()
        return reply.json().get("response", "")

    def available(self) -> tuple[bool, str]:
        try:
            reply = httpx.get(f"{self.endpoint}/api/tags", timeout=3.0)
            reply.raise_for_status()
            names = [m["name"] for m in reply.json().get("models", [])]
        except Exception as exc:  # noqa: BLE001
            return False, f"Ollama unreachable at {self.endpoint}: {exc}"

        if not any(n == self.model or n.split(":")[0] == self.model for n in names):
            return False, f"model {self.model!r} not pulled (have: {', '.join(names) or 'none'})"
        return True, "ok"

    def installed_models(self) -> list[str]:
        try:
            reply = httpx.get(f"{self.endpoint}/api/tags", timeout=3.0)
            reply.raise_for_status()
            return sorted(m["name"] for m in reply.json().get("models", []))
        except Exception:  # noqa: BLE001
            return []

    def warm_up(self) -> float:
        started = time.monotonic()
        try:
            httpx.post(
                f"{self.endpoint}/api/generate",
                json={"model": self.model, "prompt": "hi", "stream": False,
                      "think": False, "keep_alive": self.keep_alive,
                      "options": {"num_predict": 1}},
                timeout=180.0,
            ).raise_for_status()
        except Exception as exc:  # noqa: BLE001
            log.warning("could not warm up %s: %s", self.model, exc)
            return 0.0
        return time.monotonic() - started

    def unload(self) -> None:
        try:
            httpx.post(f"{self.endpoint}/api/generate",
                       json={"model": self.model, "keep_alive": 0}, timeout=10.0)
        except Exception as exc:  # noqa: BLE001
            log.debug("unload failed: %s", exc)


class AnthropicBackend:
    """Claude through the official SDK."""

    key = "anthropic"

    def __init__(self, model: str = "claude-opus-5") -> None:
        self.model = model
        self._client = None

    def _get_client(self):
        if self._client is None:
            import anthropic

            api_key = secrets.get_key("anthropic")
            if not api_key:
                raise BackendError(
                    "no Anthropic API key - set one in the settings window, "
                    "or export ANTHROPIC_API_KEY"
                )
            self._client = anthropic.Anthropic(api_key=api_key)
        return self._client

    def complete(self, system: str, prompt: str, timeout: float,
                 thinking: bool = False) -> str:
        import anthropic

        client = self._get_client()
        try:
            response = client.with_options(timeout=timeout).messages.create(
                model=self.model,
                max_tokens=MAX_TOKENS,
                system=system,
                messages=[{"role": "user", "content": prompt}],
                # Cleanup is a short rewrite and the user is waiting on it, so
                # keep thinking shallow rather than switching it off - disabling
                # it on Opus 5 can leak reasoning into the visible answer.
                output_config={"effort": "high" if thinking else "low"},
            )
        except anthropic.AuthenticationError as exc:
            raise BackendError("Anthropic rejected the API key") from exc
        except anthropic.RateLimitError as exc:
            raise BackendError("Anthropic rate limit reached") from exc
        except anthropic.APIStatusError as exc:
            raise BackendError(f"Anthropic error {exc.status_code}") from exc
        except anthropic.APIConnectionError as exc:
            raise BackendError("could not reach Anthropic") from exc

        if response.stop_reason == "refusal":
            raise BackendError("Anthropic declined to process this transcript")
        return "".join(b.text for b in response.content if b.type == "text")

    def available(self) -> tuple[bool, str]:
        if not secrets.get_key("anthropic"):
            return False, "no API key set"
        try:
            self._get_client().models.retrieve(self.model)
        except Exception as exc:  # noqa: BLE001
            return False, f"{type(exc).__name__}: {str(exc)[:60]}"
        return True, "ok"

    def installed_models(self) -> list[str]:
        try:
            # SyncPage auto-paginates when iterated, but defaults to 20 per
            # request; a larger page just means fewer round trips.
            page = self._get_client().models.list(limit=1000)
            return sorted(m.id for m in page)
        except Exception:  # noqa: BLE001
            return []

    def warm_up(self) -> float:
        return 0.0  # hosted models are always warm

    def unload(self) -> None:
        pass


class OpenAICompatibleBackend:
    """Anything speaking the OpenAI protocol.

    OpenAI itself, but also OpenRouter, DeepSeek, Groq, Together, vLLM,
    llama.cpp and LM Studio - they differ only by base URL and which key opens
    them. One implementation covers the lot, so adding a provider is a row in
    PROVIDERS rather than a new class.
    """

    def __init__(self, model: str, provider: str = "openai",
                 base_url: str | None = None) -> None:
        self.model = model
        self.key = provider
        self.base_url = base_url or (PROVIDERS[provider].base_url
                                     if provider in PROVIDERS else None)
        self._client = None

    @property
    def _label(self) -> str:
        return PROVIDERS[self.key].label if self.key in PROVIDERS else self.key

    def _get_client(self):
        if self._client is None:
            import openai

            api_key = secrets.get_key(self.key)
            if not api_key:
                env = PROVIDERS[self.key].env_var if self.key in PROVIDERS else None
                raise BackendError(
                    f"no {self._label} API key - set one in the settings "
                    f"window" + (f", or export {env}" if env else "")
                )
            if self.key == "custom" and not self.base_url:
                raise BackendError(
                    "no base URL set - a custom provider needs the address of "
                    "its OpenAI-compatible endpoint"
                )
            self._client = openai.OpenAI(api_key=api_key, base_url=self.base_url)
        return self._client

    def complete(self, system: str, prompt: str, timeout: float,
                 thinking: bool = False) -> str:
        import openai

        client = self._get_client()
        try:
            response = client.with_options(timeout=timeout).chat.completions.create(
                model=self.model,
                max_completion_tokens=MAX_TOKENS,
                messages=[
                    {"role": "system", "content": system},
                    {"role": "user", "content": prompt},
                ],
            )
        except openai.AuthenticationError as exc:
            raise BackendError(f"{self._label} rejected the API key") from exc
        except openai.RateLimitError as exc:
            raise BackendError(f"{self._label} rate limit reached") from exc
        except openai.NotFoundError as exc:
            raise BackendError(f"{self._label} has no model {self.model!r}") from exc
        except openai.APIStatusError as exc:
            raise BackendError(f"{self._label} error {exc.status_code}") from exc
        except openai.APIConnectionError as exc:
            raise BackendError(f"could not reach {self._label}") from exc

        choice = response.choices[0].message
        text = choice.content or ""
        # Reasoning models on some routers return the answer alongside a
        # separate reasoning field; only the answer should be pasted.
        return text.strip()

    def available(self) -> tuple[bool, str]:
        if not secrets.get_key(self.key):
            return False, "no API key set"
        if self.key == "custom" and not self.base_url:
            return False, "no base URL set"
        try:
            self._get_client().models.retrieve(self.model)
        except Exception as exc:  # noqa: BLE001
            return False, f"{type(exc).__name__}: {str(exc)[:60]}"
        return True, "ok"

    def _public_models(self, url: str) -> list[str]:
        try:
            reply = httpx.get(url, timeout=10.0)
            reply.raise_for_status()
            return [m["id"] for m in reply.json().get("data", []) if m.get("id")]
        except Exception:  # noqa: BLE001
            return []

    def installed_models(self) -> list[str]:
        spec = PROVIDERS.get(self.key)

        # A public catalogue means the dropdown can be filled before the user
        # has pasted a key, which is when they most want to browse it.
        if spec is not None and spec.public_models_url:
            found = self._public_models(spec.public_models_url)
            if found:
                return usable_chat_models(found, self.key)

        try:
            found = [m.id for m in self._get_client().models.list()]
        except Exception:  # noqa: BLE001
            return []
        return usable_chat_models(found, self.key)

    def warm_up(self) -> float:
        return 0.0

    def unload(self) -> None:
        pass


# Kept as a name because "the OpenAI backend" is what callers ask for.
OpenAIBackend = OpenAICompatibleBackend


def build_backend(config):
    """Construct the backend named in the config."""
    backend = config.backend
    if backend == "anthropic":
        return AnthropicBackend(config.model)
    if backend in PROVIDERS and PROVIDERS[backend].needs_api_key:
        return OpenAICompatibleBackend(
            config.model, backend,
            getattr(config, "base_url", "") or PROVIDERS[backend].base_url,
        )
    return OllamaBackend(config.model, config.endpoint, config.keep_alive)
