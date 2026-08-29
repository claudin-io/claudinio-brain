"""The adapters, which are the recipe from docs/frameworks.md with a name.

None of them imports its framework at module scope, and that is the property
under test here as much as the behaviour: `pip install claudinio-brain-hooks` in
a project that uses one framework must not fail because six others are absent.
"""

import stat

import pytest

from claudinio_brain_hooks import adapters


@pytest.fixture
def fake_brain(tmp_path, monkeypatch):
    def install(body: str) -> None:
        exe = tmp_path / "brain"
        exe.write_text(f"#!/bin/sh\n{body}\n")
        exe.chmod(exe.stat().st_mode | stat.S_IEXEC)
        monkeypatch.setenv("PATH", str(tmp_path))
        monkeypatch.delenv("BRAIN_BIN", raising=False)
        monkeypatch.delenv("BRAIN_HOOK", raising=False)

    return install


def test_the_langgraph_node_adds_a_message_or_nothing(fake_brain):
    fake_brain('echo "recorded evidence"')
    node = adapters.langgraph_node()
    state = {"messages": [{"role": "user", "content": "what is the deploy runbook"}]}
    assert node(state) == {
        "messages": [{"role": "system", "content": "recorded evidence"}]
    }

    fake_brain("true")
    assert node(state) == {}, "an empty answer adds no message at all"


def test_the_langgraph_node_survives_a_state_it_does_not_recognise(fake_brain):
    fake_brain('echo "recorded evidence"')
    node = adapters.langgraph_node()
    for state in ({}, {"messages": []}, {"messages": [object()]}):
        assert node(state) == {}, f"no answer, no exception: {state}"


def test_the_autogen_hook_is_a_string_in_a_string_out(fake_brain):
    fake_brain('echo "recorded evidence"')
    hook = adapters.autogen_hook()
    assert hook("what is the deploy runbook") == (
        "recorded evidence\n\nwhat is the deploy runbook"
    )

    fake_brain("true")
    assert hook("unchanged prompt here") == "unchanged prompt here"


def test_priming_a_crew_task_mutates_only_its_description(fake_brain):
    fake_brain('echo "recorded evidence"')

    class Task:
        description = "ship the checkout page"
        expected_output = "a diff"

    task = Task()
    adapters.prime_crew_task(task)
    assert task.description == "recorded evidence\n\nship the checkout page"
    assert task.expected_output == "a diff", "and nothing else about it"


def test_an_adapter_module_needs_none_of_its_frameworks_installed():
    import importlib
    import sys

    for name in ("crewai", "langchain_core", "langgraph", "autogen", "agents"):
        assert name not in sys.modules, f"{name} was imported at module scope"

    # And it can be re-imported from cold with the same result.
    importlib.reload(adapters)
