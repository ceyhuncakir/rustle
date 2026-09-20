"""Cleanup-layer tests against a stub Ollama, covering the failure paths.

The rule these enforce: a dictation must never be lost. Whatever the model or
the daemon does - reasoning tags, quotes, timeouts, a 500, an empty reply - the
user still gets their words.
"""

from __future__ import annotations

import json
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer

import pytest

from flowd.cleanup import Cleaner, NullCleaner, _strip_thinking, _strip_wrapping_quotes


class _Stub(BaseHTTPRequestHandler):
    reply = ""
    status = 200
    last_payload: dict = {}

    def do_POST(self):  # noqa: N802
        length = int(self.headers["Content-Length"])
        type(self).last_payload = json.loads(self.rfile.read(length))
        self.send_response(type(self).status)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps({"response": type(self).reply}).encode())

    def do_GET(self):  # noqa: N802
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps({"models": [{"name": "qwen3:14b"}]}).encode())

    def log_message(self, *_args):
        pass


@pytest.fixture
def stub():
    server = HTTPServer(("127.0.0.1", 0), _Stub)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    yield server, f"http://127.0.0.1:{server.server_port}"
    server.shutdown()


def make(endpoint, **kwargs):
    return Cleaner(endpoint=endpoint, model="qwen3:14b", timeout=5.0, **kwargs)


def test_returns_cleaned_text(stub):
    _, url = stub
    _Stub.reply = "Hey, can you look at the auth middleware after the deploy?"
    _Stub.status = 200
    out = make(url).clean("um so hey can you uh look at the auth middleware")
    assert out == "Hey, can you look at the auth middleware after the deploy?"


def test_strips_reasoning_block(stub):
    _, url = stub
    _Stub.reply = "<think>The user said um, drop it.</think>Hello there."
    _Stub.status = 200
    assert make(url).clean("um hello there") == "Hello there."


def test_strips_wrapping_quotes(stub):
    _, url = stub
    _Stub.reply = '"Hello there."'
    _Stub.status = 200
    assert make(url).clean("hello there") == "Hello there."


def test_server_error_falls_back_to_raw(stub):
    _, url = stub
    _Stub.reply = "ignored"
    _Stub.status = 500
    assert make(url).clean("um hello there") == "um hello there"


def test_empty_reply_falls_back_to_raw(stub):
    _, url = stub
    _Stub.reply = "   "
    _Stub.status = 200
    assert make(url).clean("um hello there") == "um hello there"


def test_unreachable_daemon_falls_back_to_raw():
    # Port 1 is reserved and nothing listens there.
    assert make("http://127.0.0.1:1").clean("hello there") == "hello there"


def test_context_and_dictionary_reach_the_prompt(stub):
    _, url = stub
    _Stub.reply = "ok"
    _Stub.status = 200
    cleaner = make(url, dictionary=["Ceyhun", "interopt"],
                   app_rules={"Slack": "Casual, no sign-off."})
    cleaner.clean("hello", {"app": "Slack", "title": "#general"})

    prompt = _Stub.last_payload["prompt"]
    assert "Slack" in prompt and "#general" in prompt
    assert "Casual, no sign-off." in prompt
    assert "Ceyhun" in prompt and "interopt" in prompt


def test_app_rule_only_applies_to_that_app(stub):
    _, url = stub
    _Stub.reply = "ok"
    _Stub.status = 200
    cleaner = make(url, app_rules={"Slack": "Casual, no sign-off."})
    cleaner.clean("hello", {"app": "org.gnome.TextEditor", "title": "notes"})
    assert "Casual, no sign-off." not in _Stub.last_payload["prompt"]


def test_availability_reports_missing_model(stub):
    _, url = stub
    ok, why = Cleaner(endpoint=url, model="not-pulled").available()
    assert not ok and "not pulled" in why


def test_empty_input_short_circuits():
    assert make("http://127.0.0.1:1").clean("   ") == ""


def test_null_cleaner_passes_through():
    assert NullCleaner().clean("  um hello  ") == "um hello"


@pytest.mark.parametrize(
    "raw,expected",
    [
        ("<think>a</think>b", "b"),
        ("<think>\nmulti\nline\n</think>  text  ", "text"),
        ("no tags", "no tags"),
    ],
)
def test_strip_thinking(raw, expected):
    assert _strip_thinking(raw) == expected


@pytest.mark.parametrize(
    "raw,expected",
    [('"x"', "x"), ("'x'", "x"), ('"unbalanced', '"unbalanced'), ("plain", "plain")],
)
def test_strip_quotes(raw, expected):
    assert _strip_wrapping_quotes(raw) == expected


# -- intent resolution ------------------------------------------------------
# Reasoning is the expensive path, so what triggers it matters as much as what
# it does. These pin the trigger; scripts/eval-cleanup.py checks the output
# quality against the real model.


def test_detects_retraction_cues():
    from flowd.cleanup import has_retraction_cue

    for phrase in [
        "no wait, tuesday", "actually forget that", "scratch that",
        "never mind", "let me start over", "I meant the other one",
        "put it in the top bar instead", "sorry, I meant friday",
    ]:
        assert has_retraction_cue(phrase), phrase


def test_plain_speech_has_no_retraction_cue():
    from flowd.cleanup import has_retraction_cue

    for phrase in [
        "fix the scanner timeout and add slack notifications",
        "can you look at the auth middleware before the deploy",
        "yeah it works perfect",
    ]:
        assert not has_retraction_cue(phrase), phrase


def test_bare_actually_is_not_a_retraction():
    """Measured on 44 real dictations, "actually" alone fired 11 times and was
    a genuine retraction once. Treating it as a cue costs a wasted LLM call."""
    from flowd.cleanup import has_retraction_cue

    for phrase in [
        "bro right now I'm actually typing this to you in my own project",
        "okay so it actually works and I'm testing this out",
        "I'm actually making improvements to the pipeline",
        "now I can actually write articulated sentences rather than typing",
    ]:
        assert not has_retraction_cue(phrase), phrase


def test_actually_next_to_a_negation_is_a_retraction():
    from flowd.cleanup import has_retraction_cue

    for phrase in [
        "actually no, forget the scanner",
        "I know actually wait, forget about that, use Figma",
        "no actually, use the other one",
    ]:
        assert has_retraction_cue(phrase), phrase


def long_retraction(words=20):
    filler = " ".join(["something"] * (words - 4))
    return f"I want {filler} actually no forget that"


def test_thinking_only_when_a_change_of_mind_is_plausible():
    cleaner = make("http://127.0.0.1:1", think="auto")

    # Long and contains a cue -> worth the reasoning tokens.
    assert cleaner._should_think(long_retraction())
    # A cue but far too short to be tangled.
    assert not cleaner._should_think("no wait, tuesday")
    # Long but no cue at all.
    assert not cleaner._should_think(" ".join(["word"] * 40))


def test_think_modes_override_the_heuristic():
    always = make("http://127.0.0.1:1", think="always")
    never = make("http://127.0.0.1:1", think="never")

    assert always._should_think("hi")
    assert not never._should_think(long_retraction())


def test_disabling_intent_resolution_disables_thinking():
    cleaner = make("http://127.0.0.1:1", resolve_intent=False, think="auto")
    assert not cleaner._should_think(long_retraction())


def test_reasoning_is_off_by_default():
    # It scored identically on the eval corpus while costing 10-100x the time.
    assert not make("http://127.0.0.1:1")._should_think(long_retraction())


def test_system_prompt_reflects_intent_setting():
    from flowd.cleanup import build_system_prompt

    with_intent = build_system_prompt(resolve_intent=True)
    without = build_system_prompt(resolve_intent=False)

    assert "SETTLED ON" in with_intent
    assert "SETTLED ON" not in without
    # Sections are numbered on assembly, so dropping one must renumber the
    # rest rather than leaving a gap the model has to reconcile.
    # With intent: repair, intent, grammar, language, voice.
    assert "5. PRESERVE THEIR VOICE" in with_intent
    # Without: repair, grammar, language, voice.
    assert "4. PRESERVE THEIR VOICE" in without


def test_sections_are_numbered_consecutively_from_one():
    import re

    from flowd.cleanup import build_system_prompt

    for style in ("light", "balanced", "tidy"):
        for intent in (True, False):
            prompt = build_system_prompt(style=style, resolve_intent=intent)
            numbers = [int(n) for n in re.findall(r"^(\d+)\. [A-Z]", prompt, re.M)]
            assert numbers == list(range(1, len(numbers) + 1)), (style, intent)


def test_light_style_skips_grammar_repair():
    from flowd.cleanup import build_system_prompt

    # Repairing grammar rewords things, which is exactly what "light" avoids.
    assert "WRITTEN PROSE" not in build_system_prompt(style="light")
    assert "WRITTEN PROSE" in build_system_prompt(style="balanced")
    assert "WRITTEN PROSE" in build_system_prompt(style="tidy")


def test_filler_and_discourse_words_are_both_covered():
    from flowd.cleanup import build_system_prompt

    prompt = build_system_prompt()
    # The two rules pull in opposite directions, so both must be present or
    # the model over- or under-deletes.
    assert '"like"' in prompt and "carry no meaning" in prompt
    assert "Discourse words" in prompt and "NOT filler" in prompt


def test_styles_change_the_prompt():
    from flowd.cleanup import build_system_prompt

    light = build_system_prompt(style="light")
    tidy = build_system_prompt(style="tidy")
    balanced = build_system_prompt(style="balanced")

    assert "change as little as possible" in light
    assert "tighten loose phrasing" in tidy
    assert "change as little as possible" not in balanced
    assert "tighten loose phrasing" not in balanced


def test_thinking_requests_get_a_longer_timeout(stub):
    _, url = stub
    _Stub.reply = "ok"
    _Stub.status = 200
    cleaner = make(url, think="always")
    cleaner.clean("hello there")
    assert _Stub.last_payload["think"] is True


def test_normal_requests_do_not_think(stub):
    _, url = stub
    _Stub.reply = "ok"
    _Stub.status = 200
    make(url).clean("hello there")
    assert _Stub.last_payload["think"] is False


# -- guard against the model answering instead of transcribing --------------
# Repairing tangled grammar means giving the model licence to reword, and the
# failure mode of that licence is answering the dictation. Cleanup only ever
# condenses, so growth is the tell.


def test_guard_accepts_normal_condensing():
    from flowd.cleanup import suspicious_rewrite

    raw = "um so like can you uh look at the the auth middleware before the deploy"
    assert suspicious_rewrite(raw, "Can you look at the auth middleware before the deploy?") is None


def test_guard_rejects_an_answered_question():
    from flowd.cleanup import suspicious_rewrite

    raw = "what is the capital of france"
    answer = (
        "The capital of France is Paris, which has been the country's capital "
        "since the tenth century and is home to around two million people."
    )
    assert "expanded" in suspicious_rewrite(raw, answer)


def test_guard_rejects_a_code_block():
    from flowd.cleanup import suspicious_rewrite

    raw = "write me a python function that sorts a list"
    assert "code block" in suspicious_rewrite(raw, "```python\ndef s(x): ...\n```")


def test_guard_does_not_trip_on_very_short_dictations():
    from flowd.cleanup import suspicious_rewrite

    # "yes" -> "Yes." triples the ratio but is obviously fine.
    assert suspicious_rewrite("yes", "Yes.") is None
    assert suspicious_rewrite("mm-hmm", "Mm-hmm.") is None
    assert suspicious_rewrite("ok", "Okay.") is None


def test_guard_allows_spoken_urls_to_collapse():
    from flowd.cleanup import suspicious_rewrite

    assert suspicious_rewrite("go to W W dot youtube dot com", "Go to www.youtube.com") is None


def test_clean_falls_back_to_raw_when_the_guard_fires(stub):
    _, url = stub
    _Stub.status = 200
    _Stub.reply = (
        "The capital of France is Paris, a city of roughly two million people "
        "that has served as the capital since the tenth century and remains "
        "the political and cultural centre of the country today."
    )
    raw = "what is the capital of france"
    # The transcript must survive even when the model ignores its instructions.
    assert make(url).clean(raw) == raw


# -- language handling ------------------------------------------------------
# An English system prompt quietly pulls output towards English. Measured
# before the LANGUAGE section existed, Dutch came back as English about half
# the time.


def test_polish_pass_never_translates():
    """Polish always works in the source language; translation is its own
    pass, because the model does cleanup OR translation, never both."""
    from flowd.cleanup import build_system_prompt

    prompt = build_system_prompt(languages=["en", "nl"])
    assert "SAME language" in prompt
    assert "Never translate" in prompt


def test_translate_prompt_targets_the_requested_language():
    from flowd.cleanup import LANGUAGE_NAMES, TRANSLATE_PROMPT

    dutch = TRANSLATE_PROMPT.format(target=LANGUAGE_NAMES["nl"])
    assert "into Dutch" in dutch
    # Register must survive translation or casual speech turns formal.
    assert "slang and swearing" in dutch
    assert "already in Dutch, return it unchanged" in dutch


def test_loanwords_do_not_make_a_dutch_sentence_english():
    from flowd.cleanup import build_system_prompt

    prompt = build_system_prompt(languages=["en", "nl"])
    assert "loanwords" in prompt
    assert "deployen" in prompt


def test_spoken_languages_are_named_in_the_prompt():
    from flowd.cleanup import build_system_prompt

    assert "English or Dutch" in build_system_prompt(languages=["en", "nl"])
    assert "English" in build_system_prompt(languages=["en"])


def test_no_section_heading_names_a_language():
    """A heading like "MAKE IT READ AS WRITTEN ENGLISH" biases Dutch output
    back towards English, which is exactly the drift being fixed."""
    import re

    from flowd.cleanup import build_system_prompt

    prompt = build_system_prompt(languages=["en", "nl"])
    headings = re.findall(r"^\d+\. ([A-Z][A-Z ]+)$", prompt, re.M)
    assert headings, "no headings found"
    for heading in headings:
        assert "ENGLISH" not in heading and "DUTCH" not in heading, heading


def test_resolve_pass_is_told_not_to_translate():
    from flowd.cleanup import RESOLVE_PROMPT

    assert "language" in RESOLVE_PROMPT
    assert "translating here would be wrong" in RESOLVE_PROMPT


def test_dutch_retraction_cues():
    from flowd.cleanup import has_retraction_cue

    for phrase in [
        "nee wacht, dinsdag", "vergeet de scanner", "laat maar zitten",
        "ik bedoelde vrijdag", "eigenlijk nee, doe maar iets anders",
        "in plaats daarvan de API",
    ]:
        assert has_retraction_cue(phrase), phrase


def test_dutch_do_not_forget_is_not_a_retraction():
    """"vergeet niet" means "do not forget" - the opposite of a retraction."""
    from flowd.cleanup import has_retraction_cue

    for phrase in [
        "vergeet niet de scanner te fixen",
        "ik wil de scanner fixen",
        "dit werkt echt heel goed",
    ]:
        assert not has_retraction_cue(phrase), phrase
