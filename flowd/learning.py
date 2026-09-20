"""Learn this speaker's vocabulary and voice from their own dictations.

Off unless switched on. When on, Flow builds two things from the local history
and feeds both back into the cleanup prompt:

  vocabulary - jargon, product and project names a general recogniser mangles
  style      - one instruction describing how this person actually talks

Both are mined by the same local model that does the cleanup, in the
background, never on the path between your key release and the paste.
"""

from __future__ import annotations

import json
import logging
import re

import httpx

log = logging.getLogger(__name__)

VOCAB_KEY = "vocabulary"
STYLE_KEY = "style"
# Terms the user has rejected. Mining cannot distinguish jargon from a
# mis-transcription the speaker later corrected, so the last word is theirs.
BLOCKED_KEY = "blocked"

VOCAB_PROMPT = """\
You are given transcripts of one person's dictation. Extract only the NAMES a \
general speech recogniser would get wrong: products, projects, companies, \
tools, libraries, file formats, commands and people.

Every term you return MUST be copied from the transcripts below. Do not \
invent terms, and do not repeat anything from these instructions.

Exclude, however often they appear:
- ordinary technical vocabulary any recogniser already knows - "model", \
"pipeline", "audio", "GPU", "UI", "agent", "transcript", "optimize"
- ordinary verbs and adjectives - "rephrase", "articulate", "glitch"
- anything that looks like a mis-transcription: nonsense strings, or two \
near-identical variants of the same thing. If you are unsure a term is spelled \
the way the speaker meant, leave it out.
- anything appearing only once. One mention is not a pattern.

Fewer, better terms beat a long list: every term you return is shown to \
another model as gospel spelling. At most {max_terms}, most distinctive first.

Return a JSON array of strings and nothing else."""


STYLE_PROMPT = """\
You are given transcripts of one person's dictation. Write at most two \
sentences instructing an editor how this person's WRITTEN text should read, so \
their voice survives editing.

Critical: these transcripts are speech. Filler words, "uh", "um", "like", \
stutters, repeated phrases, false starts and fragments are artefacts of \
speaking and are always removed before your instruction is applied. Never \
mention them, and never ask for them to be kept - that would undo the \
cleanup.

Describe only what should survive into the written text: register (formal or \
casual), whether they swear, characteristic words or openers, typical sentence \
length, and whether they mix languages.

For example: "This speaker is casual and direct, swears freely, favours short \
punchy sentences, and drops English technical terms into Dutch."

Return only that instruction, with no preamble."""


def _ask(endpoint: str, model: str, system: str, prompt: str, timeout: float) -> str:
    reply = httpx.post(
        f"{endpoint.rstrip('/')}/api/generate",
        json={
            "model": model,
            "system": system,
            "prompt": prompt,
            "stream": False,
            "think": False,
            "options": {"temperature": 0.2},
        },
        timeout=timeout,
    )
    reply.raise_for_status()
    text = reply.json().get("response", "")
    return re.sub(r"<think>.*?</think>", "", text, flags=re.DOTALL).strip()


def _parse_terms(text: str, max_terms: int) -> list[str]:
    """Pull a list of terms out of the reply, tolerating stray prose."""
    match = re.search(r"\[.*]", text, re.DOTALL)
    if not match:
        return []
    try:
        raw = json.loads(match.group(0))
    except json.JSONDecodeError:
        return []

    terms: list[str] = []
    seen = set()
    for item in raw:
        if not isinstance(item, str):
            continue
        term = item.strip()
        # Long "terms" are the model summarising rather than extracting.
        if not term or len(term) > 40 or term.lower() in seen:
            continue
        seen.add(term.lower())
        terms.append(term)
    return terms[:max_terms]


def verify_terms(terms: list[str], samples: list[str], min_uses: int = 2) -> list[str]:
    """Keep only terms the speaker demonstrably used.

    Models parrot their own instructions: given example terms in the prompt,
    the first version returned five names that appear nowhere in this user's
    history. Learned vocabulary is handed to another model as correct
    spelling, so a fabricated term becomes a word Flow will happily insert
    into text the user never said. Checking against the source closes that
    off regardless of what the prompt says.
    """
    corpus = "\n".join(samples).lower()
    kept = []
    for term in terms:
        needle = term.lower().strip()
        if not needle:
            continue
        uses = corpus.count(needle)
        if uses >= min_uses:
            kept.append(term)
        else:
            log.debug("dropping %r - appears %d time(s) in history", term, uses)
    return kept


def _batch(samples: list[str], limit: int = 120) -> str:
    return "\n".join(f"- {s}" for s in samples[:limit])


class Learner:
    def __init__(self, endpoint: str, model: str, timeout: float = 120.0) -> None:
        self.endpoint = endpoint
        self.model = model
        self.timeout = timeout

    def mine_vocabulary(self, samples: list[str], max_terms: int = 40) -> list[str]:
        if not samples:
            return []
        try:
            reply = _ask(
                self.endpoint, self.model,
                VOCAB_PROMPT.format(max_terms=max_terms),
                _batch(samples), self.timeout,
            )
        except Exception as exc:  # noqa: BLE001 - learning must never break dictation
            log.warning("vocabulary mining failed: %s", exc)
            return []

        proposed = _parse_terms(reply, max_terms)
        terms = verify_terms(proposed, samples)
        if len(terms) < len(proposed):
            log.info(
                "dropped %d proposed term(s) not found in the history",
                len(proposed) - len(terms),
            )
        return terms

    def profile_style(self, samples: list[str]) -> str:
        if not samples:
            return ""
        try:
            reply = _ask(self.endpoint, self.model, STYLE_PROMPT,
                         _batch(samples), self.timeout)
        except Exception as exc:  # noqa: BLE001
            log.warning("style profiling failed: %s", exc)
            return ""

        # One or two sentences; anything longer is the model rambling and will
        # only dilute the cleanup prompt.
        reply = " ".join(reply.split())
        return reply[:400]

    def refresh(self, history, max_terms: int = 40) -> tuple[list[str], str]:
        """Re-mine both halves of the profile and store them."""
        samples = history.samples_for_learning()
        if not samples:
            return [], ""

        terms = self.mine_vocabulary(samples, max_terms)
        blocked = {t.lower() for t in history.get_profile(BLOCKED_KEY, default=[]) or []}
        terms = [t for t in terms if t.lower() not in blocked]
        style = self.profile_style(samples)

        if terms:
            history.set_profile(VOCAB_KEY, terms, samples=len(samples))
        if style:
            history.set_profile(STYLE_KEY, style, samples=len(samples))

        log.info("learned %d terms and a style note from %d dictations",
                 len(terms), len(samples))
        return terms, style


def load_profile(history) -> tuple[list[str], str]:
    return (
        history.get_profile(VOCAB_KEY, default=[]) or [],
        history.get_profile(STYLE_KEY, default="") or "",
    )


def forget_term(history, term: str) -> bool:
    """Drop a term and remember never to learn it again."""
    blocked = history.get_profile(BLOCKED_KEY, default=[]) or []
    if term.lower() not in {t.lower() for t in blocked}:
        blocked.append(term)
        history.set_profile(BLOCKED_KEY, blocked)

    terms = history.get_profile(VOCAB_KEY, default=[]) or []
    kept = [t for t in terms if t.lower() != term.lower()]
    history.set_profile(VOCAB_KEY, kept, samples=len(kept))
    return len(kept) != len(terms)


def blocked_terms(history) -> list[str]:
    return history.get_profile(BLOCKED_KEY, default=[]) or []
