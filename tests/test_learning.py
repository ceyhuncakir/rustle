"""Learning: the store, the mining guards, and the toggle's blast radius.

The dangerous property here is that a learned term is handed to another model
as correct spelling. A fabricated one becomes a word Flow will insert into text
the user never said, so the guards matter more than the mining.
"""

from __future__ import annotations

import pytest

from flowd.history import History
from flowd.learning import (
    BLOCKED_KEY,
    VOCAB_KEY,
    _parse_terms,
    blocked_terms,
    forget_term,
    load_profile,
    verify_terms,
)


@pytest.fixture
def store(tmp_path):
    return History(tmp_path / "h.db")


# -- the store --------------------------------------------------------------


def test_records_and_counts(store):
    assert store.count() == 0
    store.record("um hello there", "Hello there.", {"app": "Slack", "title": "#eng"})
    assert store.count() == 1
    row = store.recent(1)[0]
    assert row["raw"] == "um hello there"
    assert row["clean"] == "Hello there."
    assert row["app"] == "Slack"


def test_learning_reads_the_cleaned_text(store):
    # Mining from raw would teach it the speaker's disfluencies.
    store.record("um so like the scanner", "The scanner.")
    assert store.samples_for_learning() == ["The scanner."]


def test_clear_removes_dictations_and_profile(store):
    store.record("a", "b")
    store.set_profile(VOCAB_KEY, ["thing"])
    assert store.clear() == 1
    assert store.count() == 0
    assert store.get_profile(VOCAB_KEY, default=[]) == []


def test_profile_round_trips(store):
    store.set_profile(VOCAB_KEY, ["Ptyxis", "interopt"], samples=42)
    assert store.get_profile(VOCAB_KEY) == ["Ptyxis", "interopt"]
    assert store.profile_meta(VOCAB_KEY)["samples"] == 42


def test_missing_profile_returns_the_default(store):
    assert store.get_profile("nope", default=[]) == []
    assert store.profile_meta("nope") is None


# -- the guard that matters -------------------------------------------------


def test_verify_drops_terms_never_actually_said():
    """Models parrot their own instructions. The first version of the prompt
    listed example terms and got five of them back, none of which appeared in
    the user's history."""
    samples = ["I use Whisper Flow every day", "Whisper Flow is great"]
    proposed = ["Whisper Flow", "PufferLib", "Ptyxis"]
    assert verify_terms(proposed, samples) == ["Whisper Flow"]


def test_verify_requires_more_than_one_use():
    samples = ["I mentioned Figma once", "nothing else here"]
    assert verify_terms(["Figma"], samples) == []
    assert verify_terms(["Figma"], samples, min_uses=1) == ["Figma"]


def test_verify_is_case_insensitive():
    samples = ["the SCANNER broke", "fix the scanner"]
    assert verify_terms(["Scanner"], samples) == ["Scanner"]


def test_verify_handles_an_empty_corpus():
    assert verify_terms(["anything"], []) == []


# -- parsing the model's reply ----------------------------------------------


def test_parses_a_json_array():
    assert _parse_terms('["a", "b"]', 10) == ["a", "b"]


def test_parses_an_array_wrapped_in_prose():
    assert _parse_terms('Sure! Here you go:\n["a", "b"]\nHope that helps', 10) == ["a", "b"]


def test_parse_survives_junk():
    assert _parse_terms("no json here", 10) == []
    assert _parse_terms("[not valid json", 10) == []


def test_parse_drops_sentences_masquerading_as_terms():
    long = "this is clearly a summary sentence and not a vocabulary term at all"
    assert _parse_terms(f'["ok", "{long}"]', 10) == ["ok"]


def test_parse_deduplicates_and_caps():
    assert _parse_terms('["a", "A", "b", "c"]', 2) == ["a", "b"]


# -- curation ---------------------------------------------------------------


def test_forgetting_removes_and_blocks(store):
    store.set_profile(VOCAB_KEY, ["Cafe", "Whisper Flow"])
    assert forget_term(store, "Cafe") is True
    assert store.get_profile(VOCAB_KEY) == ["Whisper Flow"]
    assert blocked_terms(store) == ["Cafe"]


def test_blocking_something_not_learned_still_records_it(store):
    store.set_profile(VOCAB_KEY, ["Whisper Flow"])
    assert forget_term(store, "Nonsense") is False
    assert blocked_terms(store) == ["Nonsense"]


def test_blocked_terms_are_not_relearned(store, monkeypatch):
    from flowd.learning import Learner

    store.record("the Cafe thing", "the Cafe thing and Cafe again")
    store.set_profile(BLOCKED_KEY, ["Cafe"])

    learner = Learner("http://127.0.0.1:1", "model")
    monkeypatch.setattr(learner, "mine_vocabulary", lambda s, m=40: ["Cafe"])
    monkeypatch.setattr(learner, "profile_style", lambda s: "casual")

    terms, _ = learner.refresh(store)
    assert terms == []


# -- failure must never break dictation -------------------------------------


def test_mining_survives_an_unreachable_model(store):
    from flowd.learning import Learner

    learner = Learner("http://127.0.0.1:1", "model", timeout=1.0)
    assert learner.mine_vocabulary(["something"]) == []
    assert learner.profile_style(["something"]) == ""


def test_load_profile_on_a_fresh_store(store):
    assert load_profile(store) == ([], "")
