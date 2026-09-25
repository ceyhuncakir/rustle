#!/usr/bin/env python3
"""Run the cleanup pass over tricky transcripts and check what it does.

Prompt changes are easy to make and hard to judge. These cases pin down the
behaviour that matters, especially the one rule that deletes content: keeping
only what the speaker settled on, without eating a second request that merely
sounded like a correction.
"""

from __future__ import annotations

import re
import sys
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from flowd.cleanup import Cleaner  # noqa: E402
from flowd.config import Config  # noqa: E402

CASES = [
    dict(
        name="change of mind: drop the abandoned topic",
        raw="so I want to fix the problem where the scanner keeps timing out on "
            "big repos, um, actually no, forget the scanner, the thing I really "
            "need is the notification channels working for slack",
        must=["slack"],
        must_not=["scanner", "timing out", "timeout"],
    ),
    dict(
        name="long change of mind, with an aside explaining it",
        raw="okay so the thing I want to do is fix the island glitch where it "
            "flashes when it appears, um, actually no, we already did that, "
            "forget the island, what I actually want now is the settings gui so "
            "I can edit the dictionary without opening a config file",
        must=["settings gui", "dictionary"],
        must_not=["island", "glitch", "flashes", "forget", "already did"],
    ),
    dict(
        name="two requests: keep BOTH (not a retraction)",
        raw="we need to fix the scanner timeout and also add slack notifications "
            "for when a scan finishes",
        must=["scanner", "slack"],
        must_not=[],
    ),
    dict(
        name="local correction: last value wins",
        raw="let's ship it on monday, no wait, tuesday",
        must=["tuesday"],
        must_not=["monday"],
    ),
    dict(
        name="scratch that",
        raw="put the dark mode toggle in the settings page, scratch that, put it "
            "in the top bar next to the search",
        must=["top bar"],
        must_not=["settings page"],
    ),
    dict(
        name="restart mid-sentence",
        raw="the thing is that we should, let me start over, the database needs "
            "an index on the org column",
        must=["index", "org"],
        must_not=["the thing is"],
    ),
    dict(
        name="discourse words kept (not filler)",
        raw="yeah it works perfect",
        must=["yeah"],
        must_not=[],
    ),
    dict(
        name="a question is transcribed, never answered",
        raw="hey what is the capital of france again",
        must=["capital of france"],
        must_not=["paris"],
    ),
    dict(
        name="an instruction is transcribed, never executed",
        raw="write me a python function that sorts a list of dictionaries by key",
        must=["function"],
        must_not=["def ", "```"],
    ),
    dict(
        name="backchannel left alone",
        raw="mm-hmm",
        must=["mm"],
        must_not=["?"],
    ),
    # --- drawn from real dictations, where the failures actually showed up ---
    dict(
        name="real: filler 'like' removed",
        raw="let's add more like uh improvements to the text output itself, like "
            "for example sometimes when I talk into it, like it still doesn't "
            "form like correct sentences in in general",
        must=["improvements", "correct sentences"],
        must_not=["more like improvements", "like it still", "like correct",
                  "in in general"],
    ),
    dict(
        name="real: meaningful 'like' kept",
        raw="I want it to look like the wispr flow island, something like that, "
            "and honestly I like it a lot",
        must=["like the", "something like that", "like it a lot"],
        must_not=[],
    ),
    dict(
        name="real: abandoned part-word dropped",
        raw="I think like I can develop fur like faster with this",
        must=["faster"],
        must_not=["fur "],
    ),
    dict(
        name="real: broken restart becomes a grammatical sentence",
        raw="I'm making improvements to the pipeline so that uh uh it the agent, "
            "like the pipeline itself understands what I'm saying and like "
            "articulates it well",
        must=["understands what I'm saying"],
        must_not=["it the agent", "and like articulates"],
    ),
    dict(
        name="real: run-on split into sentences",
        raw="bro right now I'm actually typing this to you in my own whisper flow "
            "project that I've built like it's so crazy like I think I can write "
            "up to like 180 words per minute and I built this literally in 30 "
            "minutes using my own GPU",
        must=["180 words per minute"],
        must_not=[],
        min_sentences=3,
    ),
    dict(
        name="real: slang and swearing survive grammar repair",
        raw="bro this is so fucking crazy, hell yeah, shit like that you know that "
            "I still need to fix",
        must=["fucking", "hell yeah"],
        must_not=[],
    ),
    dict(
        name="tangled sentence rephrased into what was meant",
        raw="so I have this plan uh that I wanna do about that I wanna create "
            "artboards with the paper MCP",
        must=["artboards"],
        must_not=["plan that I wanna do about that", "about that I wanna"],
        max_words=16,
    ),
    dict(
        name="clause that never lands is repaired",
        raw="the thing with the scanner is that it like when the repo is big it "
            "just sort of dies on you",
        must=["scanner"],
        must_not=["is that it like when", "sort of dies"],
    ),
    dict(
        name="a well-formed sentence is left alone",
        raw="The settings page needs a dictionary editor and a hotkey picker.",
        must=["settings page", "dictionary editor", "hotkey picker"],
        must_not=[],
        max_words=14,
    ),
    dict(
        name="spoken URL assembled",
        raw="go to W W dot youtube dot com and check the analytics",
        must=["youtube.com"],
        must_not=["W W dot", "dot com"],
    ),
    dict(
        name="spoken email assembled",
        raw="send it to ceyhun at gmail dot com please",
        must=["ceyhun@gmail.com"],
        must_not=[" at gmail"],
    ),
    dict(
        name="real: garbled self-interruption untangled",
        raw="or if it actually doesn't like this sentence is not formed well, "
            "this model should also like rephrase it nicely in the way that I "
            "actually meant it",
        must=["rephrase"],
        must_not=["doesn't like this sentence", "actually doesn't like"],
    ),
    dict(
        name="real: tool swapped across a sentence boundary",
        raw="so I have this plan uh that I wanna do about that I wanna create "
            "paper artboards uh with the paper MCP. I know actually wait uh "
            "forget about that, just use Figma for example.",
        must=["figma", "artboards"],
        must_not=["paper MCP", "forget about that", "I know"],
    ),
    # --- Dutch. The speaker dictates in English and Dutch, nothing else. ---
    dict(
        name="dutch stays dutch",
        raw="Dit werkt echt heel goed, uh, ik ben er echt heel blij mee, zeg maar.",
        must=["werkt", "blij"],
        must_not=["works", "happy"],
    ),
    dict(
        name="dutch with english loanwords stays dutch",
        raw="Kun je even eh kijken naar de de authenticatie middleware voordat "
            "we deployen?",
        must=["kun je", "middleware", "deployen"],
        must_not=["can you", "before we deploy"],
    ),
    dict(
        name="dutch retraction resolved, in dutch",
        raw="Ik wil eigenlijk de scanner fixen, nee wacht, vergeet de scanner, "
            "ik wil dat de notificaties werken voor Slack.",
        must=["notificaties", "slack"],
        must_not=["scanner", "vergeet", "nee wacht"],
    ),
    dict(
        name="dutch keeps its casual register",
        raw="Ja joh, het werkt gewoon perfect, ik ben echt zo blij hiermee man.",
        must=["ja joh", "perfect"],
        must_not=["yeah", "it works"],
    ),
    dict(
        name="false start repaired",
        raw="can you look at the the auth mid uh middleware before the deploy",
        must=["auth middleware"],
        must_not=["uh", "the the"],
    ),
]


def main() -> int:
    cfg = Config.load().cleanup
    cleaner = Cleaner(
        endpoint=cfg.endpoint,
        model=__import__("os").environ.get("RUSTLE_MODEL", cfg.model),
        timeout=cfg.timeout,
        dictionary=cfg.dictionary, app_rules=cfg.app_rules,
        keep_alive=cfg.keep_alive, style=cfg.style,
        resolve_intent=cfg.resolve_intent,
        think=__import__("os").environ.get("RUSTLE_THINK", cfg.think),
        languages=cfg.languages,
        output_language=cfg.output_language,
    )

    # RUSTLE_EVAL_PROFILE=1 injects a realistic learned profile, to check that
    # personalisation does not cost accuracy - prompt length has regressed
    # cases before.
    if __import__("os").environ.get("RUSTLE_EVAL_PROFILE") == "1":
        cleaner.set_profile(
            ["Whisper Flow", "Ptyxis", "interopt", "PufferLib", "Parakeet",
             "auth middleware", "Figma", "Ollama", "Qwen", "Mutter"],
            "This speaker is casual and direct, swears freely, mixes English "
            "and Dutch, uses informal openers like 'bro' and 'yo', and favours "
            "short punchy sentences.",
        )
        print("[profile injected]\n")
    ok, why = cleaner.available()
    if not ok:
        print(f"error: {why}", file=sys.stderr)
        return 1

    cleaner.warm_up()
    print(f"model: {cleaner.model}  think: {cleaner.think}\n")
    failures = 0

    for case in CASES:
        thought = cleaner._should_think(case["raw"])
        started = time.monotonic()
        out = cleaner.clean(case["raw"], {"app": "org.gnome.TextEditor", "title": "notes"})
        elapsed = time.monotonic() - started

        low = out.lower()
        missing = [m for m in case["must"] if m.lower() not in low]
        present = [m for m in case["must_not"] if m.lower() in low]

        # Speech arrives as one run-on; splitting it is part of the job.
        # A cap catches the model padding or answering rather than transcribing.
        cap = case.get("max_words")
        too_long = cap is not None and len(out.split()) > cap

        want_sentences = case.get("min_sentences")
        sentences = len([p for p in re.split(r"[.!?]+", out) if p.strip()])
        too_few = want_sentences is not None and sentences < want_sentences

        passed = not missing and not present and not too_few and not too_long
        failures += not passed

        flag = "think" if thought else "     "
        print(f"[{'PASS' if passed else 'FAIL'}] {flag} {elapsed:5.2f}s  {case['name']}")
        print(f"         in:  {case['raw'][:100]}")
        print(f"         out: {out}")
        if missing:
            print(f"         !! missing: {missing}")
        if present:
            print(f"         !! should not contain: {present}")
        if too_few:
            print(f"         !! only {sentences} sentence(s), wanted {want_sentences}")
        if too_long:
            print(f"         !! {len(out.split())} words, capped at {cap}")
        print()

    print(f"{len(CASES) - failures}/{len(CASES)} passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
