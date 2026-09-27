#!/usr/bin/env python3
"""Replay authored semantic traces against the real binary, without a golden oracle.

CCTAB_TEST_BIN=/path/to/tabstatus python3 tests/test_state_contract.py -v
Fixtures state the expected state and output after *every* event. This runner
only decodes persistence and observes dry-run output; it does not implement the
transition rules. Dry run observes paint decisions, not terminal delivery or
a tmux carrier's independent decay clock.
"""
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
BIN = Path(os.environ.get("CCTAB_TEST_BIN", ROOT / "bin/tabstatus")).resolve()
FIXTURE = ROOT / "tests/fixtures/state-contract-v1.json"
STATES = {"working", "waiting", "idle"}
EDGES = {"working", "waiting", "idle", "notify", "session-start", "session-end",
         "subagent-stop", "elicitation", "elicitation-result"}
OWNERS = {"main", "unknown-permission", "anonymous-permission", "notification-mcp", "anonymous-direct-mcp",
          "overflow", "direct-mcp"}
EXPECT_FIELDS = {"record", "base", "waits", "tombstones", "logical", "paint", "displayed"}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def validate_owners(entries, context, completed=False):
    require(isinstance(entries, list), context + ": expected a list")
    identities = set()
    for entry in entries:
        require(isinstance(entry, dict), context + ": expected an owner object")
        owner = entry.get("owner", "")
        direct = owner == "direct-mcp"
        fields = {"owner", "raised", "server", "id"} if direct else {"owner", "raised"}
        require(set(entry) == fields, context + ": incomplete or extra owner fields")
        require(owner in OWNERS or re.fullmatch(r"agent:[A-Za-z0-9_-]{1,64}", owner),
                context + ": invalid semantic owner")
        require(owner != "agent:-", context + ": reserved identity is not an agent")
        require(not completed or direct, context + ": only exact requests have tombstones")
        require(type(entry["raised"]) is int and entry["raised"] >= 0,
                context + ": invalid epoch")
        if direct:
            require(all(isinstance(entry[k], str) and entry[k] for k in ("server", "id")),
                    context + ": direct requests need server and id")
        identity = json.dumps({k: v for k, v in entry.items() if k != "raised"}, sort_keys=True)
        require(identity not in identities, context + ": repeated semantic owner")
        identities.add(identity)


def validate_fixture(fixture):
    require(set(fixture) == {"schema_version", "description", "scenarios"}, "invalid fixture fields")
    require(fixture["schema_version"] == 1, "unsupported fixture schema")
    require(isinstance(fixture["scenarios"], list) and fixture["scenarios"], "no scenarios")
    ids = set()
    for scenario in fixture["scenarios"]:
        sid = scenario.get("id", "")
        require(re.fullmatch(r"[a-z][a-z0-9_]+", sid) and sid not in ids, "invalid/repeated scenario id")
        ids.add(sid)
        require(set(scenario) == {"id", "description", "provenance", "env", "steps"}, sid + ": invalid fields")
        provenance = scenario["provenance"]
        require(set(provenance) == {"kind", "source", "claude_version", "conditions"}, sid + ": incomplete provenance")
        require(provenance["kind"] in {"synthetic", "capture-derived-reconstruction"}, sid + ": invalid provenance")
        require(all(isinstance(v, str) and v for v in provenance.values()), sid + ": empty provenance")
        require(set(scenario["env"]) <= {"CCTAB_TTL_WAITING"}, sid + ": environment must stay isolated")
        require(isinstance(scenario["steps"], list) and scenario["steps"], sid + ": no steps")
        for index, step in enumerate(scenario["steps"]):
            context = f"{sid}[{index}]"
            require(set(step) <= {"name", "now", "edge", "payload", "raw", "observation", "env", "expect",
                                   "capture_offset_seconds"}, context + ": unknown step field")
            require({"name", "now", "expect"} <= set(step), context + ": incomplete step")
            require(isinstance(step["name"], str) and step["name"], context + ": unnamed step")
            require(type(step["now"]) is int and step["now"] >= 0, context + ": invalid clock")
            require(set(step.get("env", {})) <= {"CCTAB_TTL_WAITING"}, context + ": invalid environment")
            if "observation" in step:
                require(step["observation"] is True and not {"edge", "payload", "raw"} & set(step),
                        context + ": an observation must invoke no hook")
            else:
                require(step.get("edge") in EDGES, context + ": invalid edge")
                require(("payload" in step) != ("raw" in step), context + ": specify exactly one input")
                require(isinstance(step.get("payload", {}), dict) and isinstance(step.get("raw", ""), str),
                        context + ": invalid input")
            expected = step["expect"]
            require(set(expected) == EXPECT_FIELDS, context + ": every semantic expectation is required")
            require(expected["record"] in {"present", "absent"}, context + ": invalid record presence")
            require(expected["base"] in STATES and expected["logical"] in STATES, context + ": invalid state")
            require(expected["paint"] in STATES | {"silent", "clear"}, context + ": invalid paint")
            require(expected["displayed"] in STATES | {"unpainted", "cleared"}, context + ": invalid display")
            validate_owners(expected["waits"], context + ".waits")
            validate_owners(expected["tombstones"], context + ".tombstones", completed=True)
            require(len(expected["waits"]) <= 9 and len(expected["tombstones"]) <= 8,
                    context + ": expectation exceeds bounded record")
    return fixture


def decode_owner(token):
    """Decode cts4's private wire spelling, without transition or expiry logic."""
    owner, epoch = token.rsplit(":", 1)
    aliases = {"-": "main", "?": "unknown-permission", "?p": "anonymous-permission", "?!": "notification-mcp",
               "!?": "anonymous-direct-mcp", "!+": "overflow"}
    if owner in aliases:
        decoded = {"owner": aliases[owner]}
    elif owner.startswith("!"):
        server, request = owner[1:].split(".")
        decoded = {"owner": "direct-mcp", "server": bytes.fromhex(server).decode(),
                   "id": bytes.fromhex(request).decode()}
    else:
        require(re.fullmatch(r"[A-Za-z0-9_-]{1,64}", owner), "invalid wire owner")
        decoded = {"owner": "agent:" + owner}
    decoded["raised"] = int(epoch)
    return decoded


def decode_record(path):
    if not path.exists():
        return {"record": "absent", "base": "idle", "waits": [], "tombstones": [], "logical": "idle"}
    lines = path.read_text().splitlines()
    require(lines and lines[0] == "cts4", "expected a complete cts4 record")
    fields = {}
    for line in lines[1:]:
        key, value = line.split(" ", 1)
        require(key in {"b", "w", "e"} and key not in fields, "unexpected/duplicate wire field")
        fields[key] = value
    require(fields.get("b") in {"w", "a", "i"}, "missing/invalid base")
    base = {"w": "working", "a": "waiting", "i": "idle"}[fields["b"]]
    waits = [decode_owner(word) for word in fields.get("w", "").split()]
    tombstones = [decode_owner(word) for word in fields.get("e", "").split()]
    return {"record": "present", "base": base, "waits": waits, "tombstones": tombstones,
            "logical": "waiting" if waits else base}


def observe_paint(output):
    if output == b"":
        return "silent"
    if output == b"\n":
        return "clear"
    for state in STATES:
        if output.startswith(state.upper().encode() + b" ") and output.endswith(b"\n"):
            return state
    raise AssertionError(f"unexpected dry-run output: {output!r}")


def semantic_order(entries):
    # Ownership is a set; vector/wire ordering is not part of these assertions.
    return sorted(entries, key=lambda entry: json.dumps(entry, sort_keys=True))


class StateContractTests(unittest.TestCase):
    def replay(self, scenario):
        with tempfile.TemporaryDirectory(prefix="cctab-contract-") as tmp:
            root = Path(tmp)
            state = root / "state"
            env = {"PATH": "/usr/bin:/bin", "HOME": tmp, "CLAUDE_PID": "0",
                   "CCTAB_STATE_DIR": str(state), "CCTAB_DRY_RUN": "1",
                   "CCTAB_TERMINAL": "other", "CCTAB_GLYPH_POS": "prefix",
                   "CCTAB_GLYPH_WORKING": "WORKING", "CCTAB_GLYPH_WAITING": "WAITING",
                   "CCTAB_GLYPH_IDLE": "IDLE", **scenario["env"]}
            displayed = "unpainted"
            for index, step in enumerate(scenario["steps"]):
                with self.subTest(trace=scenario["id"], step=index, event=step["name"]):
                    env.update(step.get("env", {}))
                    env["CCTAB_NOW"] = str(step["now"])
                    if step.get("observation"):
                        output = b""
                    else:
                        data = step["raw"].encode() if "raw" in step else json.dumps(step["payload"]).encode()
                        result = subprocess.run([str(BIN), step["edge"]], input=data, env=env, cwd=root,
                                                capture_output=True, timeout=10)
                        self.assertEqual((result.returncode, result.stderr), (0, b""))
                        output = result.stdout
                    paint = observe_paint(output)
                    if paint != "silent":
                        displayed = "cleared" if paint == "clear" else paint
                    actual = {**decode_record(state / "s1"), "paint": paint, "displayed": displayed}
                    expected = dict(step["expect"])
                    for key in ("waits", "tombstones"):
                        actual[key] = semantic_order(actual[key])
                        expected[key] = semantic_order(expected[key])
                    self.assertEqual(actual, expected)
                    # Nested or stringified metadata must never create another session.
                    self.assertEqual(sorted(p.name for p in state.iterdir()) if state.exists() else [],
                                     ["s1"] if expected["record"] == "present" else [])


FIXTURES = validate_fixture(json.loads(FIXTURE.read_text()))
for _scenario in FIXTURES["scenarios"]:
    def _test(self, scenario=_scenario):
        self.replay(scenario)
    _test.__doc__ = _scenario["description"]
    setattr(StateContractTests, "test_" + _scenario["id"], _test)

if __name__ == "__main__":
    unittest.main()
