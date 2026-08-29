"""What the package promises, which is mostly what it promises *not* to do.

The Rust hooks are held to three rules -- never fail, never write, never guess
the event -- and the first of those is the one a Python callback can break. A
framework callback that raises does not degrade the agent, it stops the turn.

So most of what is tested here is silence: the ways `brain` can be missing,
broken, slow or switched off, and the fact that every one of them leaves the
prompt exactly as it was.
"""

import stat

import pytest

from claudinio_brain_hooks import core


@pytest.fixture
def fake_brain(tmp_path, monkeypatch):
    """A `brain` on PATH that this test controls.

    Returns a function that installs a shell script body; every test that needs
    a different `brain` writes its own.
    """

    def install(body: str) -> None:
        exe = tmp_path / "brain"
        exe.write_text(f"#!/bin/sh\n{body}\n")
        exe.chmod(exe.stat().st_mode | stat.S_IEXEC)
        monkeypatch.setenv("PATH", str(tmp_path))
        monkeypatch.delenv("BRAIN_BIN", raising=False)
        monkeypatch.delenv("BRAIN_HOOK", raising=False)

    return install


def test_recall_returns_what_the_brain_said(fake_brain):
    fake_brain('echo "auth strategy server-side sessions"')
    assert core.recall("qual a estrategia de auth") == "auth strategy server-side sessions"


def test_the_prompt_crosses_as_an_argument_not_a_shell_string(fake_brain):
    # If this were interpolated into a shell, the backtick and the semicolon
    # would be the bug -- and it would be a silent one, because a hook that
    # fails returns nothing and nothing looks like a brain with nothing to say.
    fake_brain('printf "%s" "$6"')
    hostile = 'what about `rm -rf /`; drop table facts--'
    assert core.recall(hostile) == hostile


def test_a_missing_binary_is_not_an_error(tmp_path, monkeypatch):
    monkeypatch.setenv("PATH", str(tmp_path))
    monkeypatch.delenv("BRAIN_BIN", raising=False)
    assert core.recall("anything at all, at length") == ""


def test_a_failing_brain_is_silent(fake_brain):
    fake_brain('echo "boom" >&2; exit 1')
    assert core.recall("anything at all, at length") == ""


def test_a_hanging_brain_does_not_hang_the_agent(fake_brain):
    fake_brain("sleep 30")
    assert core.recall("anything at all, at length", timeout=0.5) == ""


def test_the_off_switch_is_in_the_environment(fake_brain, monkeypatch):
    fake_brain('echo "this should never be read"')
    monkeypatch.setenv("BRAIN_HOOK", "off")
    assert core.recall("anything at all, at length") == ""
    assert core.context() == ""


def test_the_binary_can_be_named_outright(tmp_path, monkeypatch):
    exe = tmp_path / "brain-somewhere-else"
    exe.write_text('#!/bin/sh\necho "found me"\n')
    exe.chmod(exe.stat().st_mode | stat.S_IEXEC)
    monkeypatch.setenv("PATH", str(tmp_path / "empty"))
    monkeypatch.setenv("BRAIN_BIN", str(exe))
    monkeypatch.delenv("BRAIN_HOOK", raising=False)
    assert core.recall("anything at all, at length") == "found me"


def test_context_and_flush_ask_for_their_own_subcommands(fake_brain):
    fake_brain('printf "%s" "$2"')
    assert core.context() == "context"
    assert core.flush() == "flush"


def test_nothing_to_say_is_the_empty_string(fake_brain):
    fake_brain("true")
    assert core.recall("anything at all, at length") == ""
    assert core.prepend("anything at all, at length") == "anything at all, at length"


def test_prepend_puts_the_evidence_before_the_prompt(fake_brain):
    fake_brain('echo "the brain already holds this"')
    assert core.prepend("what do we use") == (
        "the brain already holds this\n\nwhat do we use"
    )
