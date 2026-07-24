#!/usr/bin/env python3
"""Pin Python safety-suite ownership and release-gate reachability."""

from __future__ import annotations

import ast
import re
import shlex
import unittest
from collections import deque
from pathlib import Path


SCRIPT_DIR = Path(__file__).resolve().parent
PROJECT_ROOT = SCRIPT_DIR.parent
REPO_ROOT = PROJECT_ROOT.parent.parent
RUNNER = SCRIPT_DIR / "run_python_safety_tests.py"
OFFLINE_GATE = SCRIPT_DIR / "ci_offline_gates.sh"
RUN_ALL_GATE = SCRIPT_DIR / "run_all_gates.sh"
MAKEFILE = PROJECT_ROOT / "Makefile"
WORKFLOW = REPO_ROOT / ".github/workflows/dcentos-python-safety-tests.yml"

CALL_POSITION = (
    r"^(?:if\s+)?"
    r"(?:(?:[A-Za-z_][A-Za-z0-9_]*)=[\"']?\$\()?"
    r"(?:exec\s+)?"
)
SHELL_TEST_CALL = re.compile(
    CALL_POSITION
    + r"(?:sh|bash)\s+[\"']?(?:scripts/|\$(?:SCRIPT_DIR|DIR)/)?"
    r"(?P<name>test_[A-Za-z0-9_]+\.sh)"
)
PYTHON_TEST_CALL = re.compile(
    CALL_POSITION
    + r"(?:python3\b|python\b|py\s+-3\b|run_python_script\b|"
    r"[\"']?\$PYTHON[\"']?)\s+"
    r"(?:-m\s+pytest\s+(?:-q\s+)?)?"
    r"[\"']?(?:scripts/|\$(?:SCRIPT_DIR|DIR)/)?"
    r"(?P<name>test_[A-Za-z0-9_]+\.py)"
)


def active_lines(path: Path) -> tuple[str, ...]:
    return tuple(
        line
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    )


def shell_command_segments(source: str) -> tuple[str, ...]:
    """Return active shell-command segments, excluding comments and echo text."""

    segments: list[str] = []
    for raw_line in source.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("@"):
            line = line[1:].lstrip()
        if line.endswith("\\"):
            line = line[:-1].rstrip()
        for segment in re.split(r"\s*(?:&&|;)\s*", line):
            segment = segment.strip()
            if not segment:
                continue
            try:
                first = shlex.split(segment, posix=True)[0]
            except (ValueError, IndexError):
                first = ""
            if first in {"echo", "printf"}:
                continue
            segments.append(segment)
    return tuple(segments)


def shell_logical_commands(source: str) -> tuple[str, ...]:
    """Join backslash-continued recipe lines without discarding connectors."""

    commands: list[str] = []
    pending: list[str] = []
    for raw_line in source.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("@"):
            line = line[1:].lstrip()
        continued = line.endswith("\\")
        if continued:
            line = line[:-1].rstrip()
        pending.append(line)
        if not continued:
            commands.append(" ".join(pending))
            pending = []
    if pending:
        commands.append(" ".join(pending))
    return tuple(commands)


def shell_logical_lines(source: str) -> tuple[str, ...]:
    """Join shell continuation lines before status-propagation analysis."""

    logical_lines: list[str] = []
    pending: list[str] = []
    for raw_line in source.splitlines():
        line = raw_line.strip()
        if not pending and (not line or line.startswith("#")):
            continue
        continued = line.endswith("\\")
        if continued:
            line = line[:-1].rstrip()
        pending.append(line)
        if not continued:
            joined = " ".join(part for part in pending if part).strip()
            if joined and not joined.startswith("#"):
                logical_lines.append(joined)
            pending = []
    if pending:
        joined = " ".join(part for part in pending if part).strip()
        if joined and not joined.startswith("#"):
            logical_lines.append(joined)
    return tuple(logical_lines)


def shell_tokens(command: str) -> tuple[str, ...]:
    """Tokenize one simple shell command, dropping comments and redirections safely."""

    try:
        return tuple(shlex.split(command, comments=True, posix=True))
    except ValueError:
        return ()


def command_disables_errexit(command: str) -> bool:
    tokens = shell_tokens(command)
    flattened = " ".join(tokens)
    return bool(
        re.search(
            r"(?:^|\s)set\s+\+[A-Za-z]*e[A-Za-z]*"
            r"(?=\s|[0-9]*[<>]|&>|$)",
            flattened,
        )
        or re.search(
            r"(?:^|\s)set\s+\+o\s+errexit(?=\s|[0-9]*[<>]|&>|$)",
            flattened,
        )
    )


def errexit_enabled_before(lines: tuple[str, ...], index: int) -> bool:
    """Require an unconditional prologue `set -e` that is never disabled."""

    if index == 0:
        return False
    prologue_tokens = shell_tokens(lines[0])
    enabled = bool(
        len(prologue_tokens) >= 2
        and prologue_tokens[0] == "set"
        and (
            bool(re.fullmatch(r"-[A-Za-z]*e[A-Za-z]*", prologue_tokens[1]))
            or prologue_tokens[1:] == ("-o", "errexit")
        )
        and not any(token in {"&&", "||", ";"} for token in prologue_tokens)
    )
    if not enabled:
        return False

    for raw_line in lines[1:index]:
        for command in re.split(r"\s*(?:&&|\|\||;)\s*", raw_line.strip()):
            if command_disables_errexit(command):
                return False
    return True


def conditional_start_index(lines: tuple[str, ...], index: int) -> int | None:
    """Find the `if` owning a possibly continued condition line."""

    if lines[index].strip().startswith("if "):
        return index
    cursor = index - 1
    while cursor >= 0:
        previous = lines[cursor].strip()
        if not previous.endswith(("&&", "||", "\\")):
            return None
        if previous.startswith("if "):
            return cursor
        cursor -= 1
    return None


def conditional_failure_branch_fails(
    lines: tuple[str, ...], start_index: int
) -> bool:
    """Require a top-level `else` that records or returns a failure."""

    depth = 0
    in_failure_branch = False
    failure_seen = False
    for raw_line in lines[start_index:]:
        line = raw_line.strip()
        if re.match(r"^if\b", line):
            depth += 1
        if depth == 1 and line == "else":
            in_failure_branch = True
            continue
        if in_failure_branch and depth == 1 and re.match(
            r"^(?:fail\b|exit\s+[1-9][0-9]*\b|return\s+[1-9][0-9]*\b)", line
        ):
            failure_seen = True
        if line == "fi":
            depth -= 1
            if depth == 0:
                return in_failure_branch and failure_seen
    return False


def propagated_test_calls(source: str, pattern: re.Pattern[str]) -> tuple[str, ...]:
    """Return test calls whose failure cannot be discarded by their shell path."""

    lines = shell_logical_lines(source)
    calls: list[str] = []
    for index, raw_line in enumerate(lines):
        line = raw_line.strip()
        if line.startswith("@"):
            line = line[1:].lstrip()
        match = pattern.search(line)
        if match is None:
            continue

        suffix = line[match.end() :]
        if "||" in suffix or re.search(r"(?<!\|)\|(?!\|)", suffix):
            continue
        if re.search(r"(?<![>&])&(?![>&])", suffix):
            continue

        condition_start = conditional_start_index(lines, index)
        if condition_start is not None:
            if conditional_failure_branch_fails(lines, condition_start):
                calls.append(match.group("name"))
            continue

        if "&&" in suffix:
            continue

        invoked_with_exec = bool(re.match(r"^(?:if\s+)?exec\s+", line))
        if invoked_with_exec or errexit_enabled_before(lines, index):
            calls.append(match.group("name"))
    return tuple(calls)


def workflow_trigger_paths(event: str) -> tuple[str, ...]:
    lines = WORKFLOW.read_text(encoding="utf-8").splitlines()
    event_marker = f"  {event}:"
    try:
        event_start = lines.index(event_marker) + 1
    except ValueError as error:
        raise AssertionError(f"workflow event is missing: {event}") from error

    event_lines: list[str] = []
    for line in lines[event_start:]:
        if re.match(r"^  \S", line):
            break
        if line.strip() and not line.lstrip().startswith("#"):
            event_lines.append(line)

    try:
        paths_start = next(
            index for index, line in enumerate(event_lines) if line.strip() == "paths:"
        ) + 1
    except StopIteration as error:
        raise AssertionError(f"workflow event has no paths filter: {event}") from error

    paths: list[str] = []
    for line in event_lines[paths_start:]:
        stripped = line.strip()
        if not stripped.startswith("-"):
            break
        paths.append(stripped[1:].strip().strip("\"'"))
    return tuple(paths)


def workflow_run_commands() -> tuple[str, ...]:
    lines = WORKFLOW.read_text(encoding="utf-8").splitlines()
    commands: list[str] = []
    index = 0
    while index < len(lines):
        line = lines[index]
        stripped = line.strip()
        if not stripped or stripped.startswith("#") or not stripped.startswith("run:"):
            index += 1
            continue

        indent = len(line) - len(line.lstrip())
        value = stripped.removeprefix("run:").strip()
        if value and value != "|":
            commands.append(value)
            index += 1
            continue

        index += 1
        while index < len(lines):
            child = lines[index]
            child_indent = len(child) - len(child.lstrip())
            if child.strip() and child_indent <= indent:
                break
            child_value = child.strip()
            if child_value and not child_value.startswith("#"):
                commands.append(child_value)
            index += 1
    return tuple(commands)


def runner_manifest() -> tuple[str, ...]:
    tree = ast.parse(RUNNER.read_text(encoding="utf-8"), filename=str(RUNNER))
    for node in tree.body:
        if isinstance(node, ast.Assign) and any(
            isinstance(target, ast.Name) and target.id == "SUITES"
            for target in node.targets
        ):
            value = ast.literal_eval(node.value)
            if not isinstance(value, tuple) or not all(
                isinstance(item, str) for item in value
            ):
                raise AssertionError("SUITES must be a literal tuple of strings")
            return value
    raise AssertionError("run_python_safety_tests.py has no literal SUITES manifest")


def reachable_shell_tests() -> set[Path]:
    reachable = {OFFLINE_GATE.resolve()}
    pending = deque(reachable)
    while pending:
        caller = pending.popleft()
        source = caller.read_text(encoding="utf-8")
        for name in propagated_test_calls(source, SHELL_TEST_CALL):
            callee = (SCRIPT_DIR / name).resolve()
            if callee.is_file() and callee not in reachable:
                reachable.add(callee)
                pending.append(callee)
    return reachable


def python_tests_reachable_from_offline_gate() -> set[str]:
    reachable: set[str] = set()
    for shell_test in reachable_shell_tests():
        source = shell_test.read_text(encoding="utf-8")
        reachable.update(propagated_test_calls(source, PYTHON_TEST_CALL))
    return reachable


def make_target_body(name: str) -> str:
    lines = MAKEFILE.read_text(encoding="utf-8").splitlines()
    marker = f"{name}:"
    try:
        start = lines.index(marker) + 1
    except ValueError as error:
        raise AssertionError(f"Makefile target is missing: {name}") from error

    body: list[str] = []
    for line in lines[start:]:
        if line.startswith("\t"):
            body.append(line)
        elif not line.strip() and body:
            break
        elif line.strip() and not line.lstrip().startswith("#"):
            break
    return "\n".join(body)


class PythonSafetyGateWiringTests(unittest.TestCase):
    def test_every_python_test_has_one_active_owner(self) -> None:
        known = {
            path.relative_to(SCRIPT_DIR).as_posix()
            for path in SCRIPT_DIR.rglob("test_*.py")
        }
        manifest = runner_manifest()
        runner_owned = set(manifest)
        offline_owned = python_tests_reachable_from_offline_gate()

        self.assertEqual(len(manifest), len(runner_owned), "duplicate runner suite")
        self.assertTrue(
            runner_owned.isdisjoint(offline_owned),
            f"Python suites have multiple owners: {sorted(runner_owned & offline_owned)}",
        )
        self.assertEqual(
            known,
            runner_owned | offline_owned,
            "every scripts/test_*.py must be owned by the explicit runner or an "
            "actively invoked ci_offline_gates.sh shell path",
        )
        self.assertTrue(
            all((SCRIPT_DIR / suite).is_file() for suite in runner_owned),
            "runner manifest names a missing suite",
        )

    def test_release_make_path_includes_python_and_dashboard(self) -> None:
        make_source = MAKEFILE.read_text(encoding="utf-8")
        self.assertNotRegex(
            make_source,
            re.compile(r"(?m)^[ \t]*\.IGNORE\s*:"),
            "release gates must never inherit GNU Make error suppression",
        )
        self.assertNotRegex(
            make_source,
            re.compile(
                r"(?m)^[ \t]*(?:(?:override|export|private)\s+)*"
                r"MAKEFLAGS\s*(?:[+:?!]?=)"
            ),
            "the project Makefile must not mutate inherited Make error semantics",
        )
        critical_targets = (
            "release",
            "verify",
            "test-python-safety",
            "test-dashboard",
        )
        for target in critical_targets:
            definitions = re.findall(
                rf"(?m)^{re.escape(target)}[ \t]*(?::|&:)", make_source
            )
            self.assertEqual(
                len(definitions),
                1,
                f"release-critical Make target must have one definition: {target}",
            )
        phony_targets: set[str] = set()
        for match in re.finditer(r"(?m)^\.PHONY\s*:\s*(?P<targets>[^#\n]+)", make_source):
            phony_targets.update(match.group("targets").split())
        self.assertTrue(
            {"release", "verify", "test-python-safety", "test-dashboard"}
            <= phony_targets,
            "release-critical gates must remain phony",
        )

        release = shell_command_segments(make_target_body("release"))
        self.assertIn("$(MAKE) verify", release)

        verify = shell_command_segments(make_target_body("verify"))
        self.assertIn("$(MAKE) test-python-safety", verify)
        self.assertIn("$(MAKE) test-dashboard", verify)

        python_target = shell_command_segments(make_target_body("test-python-safety"))
        self.assertTrue(
            any(
                segment.startswith('"$$PYTHON_BIN"')
                and "test_python_safety_gate_wiring.py" in segment
                for segment in python_target
            )
        )
        python_body = make_target_body("test-python-safety")
        recipe_lines = python_body.splitlines()
        self.assertTrue(recipe_lines)
        self.assertNotRegex(recipe_lines[0].lstrip("\t"), r"^[+@]*-")
        logical_commands = shell_logical_commands(python_body)
        self.assertEqual(len(logical_commands), 1)
        python_recipe = logical_commands[0]
        self.assertRegex(
            python_recipe,
            re.compile(
                r'(?:^|;\s*)"\$\$PYTHON_BIN"\s+"[^"\n]*test_python_safety_gate_wiring\.py"'
                r'\s*&&\s*'
                r'"\$\$PYTHON_BIN"\s+"[^"\n]*run_python_safety_tests\.py"'
                r'\s*$'
            ),
        )
        self.assertTrue(
            any(
                segment.startswith('"$$PYTHON_BIN"')
                and "run_python_safety_tests.py" in segment
                for segment in python_target
            )
        )

        dashboard_target = shell_command_segments(make_target_body("test-dashboard"))
        self.assertIn("npm run build", dashboard_target)
        self.assertIn("npm test", dashboard_target)

    def test_run_all_fast_path_keeps_python_safety(self) -> None:
        source = "\n".join(active_lines(RUN_ALL_GATE))
        gate_block = re.compile(
            r'if have python3; then\s+'
            r'run_gate "dcentos-python-safety" python3 "\$SCRIPT_DIR/run_python_safety_tests\.py"\s+'
            r'elif have python; then\s+'
            r'run_gate "dcentos-python-safety" python "\$SCRIPT_DIR/run_python_safety_tests\.py"\s+'
            r'else\s+'
            r"printf 'ERROR: python3 or python is required for dcentos-python-safety\\n' >&2\s+"
            r'run_gate "dcentos-python-safety" false\s+'
            r'fi',
        )
        slow_gate = 'if [ "$FAST" -eq 0 ]; then'
        match = gate_block.search(source)
        self.assertIsNotNone(match)
        self.assertIn(slow_gate, source)
        self.assertLess(match.start(), source.index(slow_gate))

    def test_workflow_runs_runner_for_every_project_change(self) -> None:
        for event in ("push", "pull_request"):
            self.assertIn("DCENT_OS_Antminer/**", workflow_trigger_paths(event))

        commands = workflow_run_commands()
        self.assertIn(
            "python DCENT_OS_Antminer/scripts/test_python_safety_gate_wiring.py",
            commands,
        )
        self.assertIn(
            "python DCENT_OS_Antminer/scripts/run_python_safety_tests.py",
            commands,
        )
        self.assertLess(
            commands.index(
                "python DCENT_OS_Antminer/scripts/test_python_safety_gate_wiring.py"
            ),
            commands.index(
                "python DCENT_OS_Antminer/scripts/run_python_safety_tests.py"
            ),
        )
        suppression_metadata = [
            line.strip()
            for line in WORKFLOW.read_text(encoding="utf-8").splitlines()
            if re.match(
                r'''^\s*(?:["']?)(?:if|continue-on-error|shell)(?:["']?)\s*:''',
                line,
            )
        ]
        self.assertEqual(
            suppression_metadata,
            [],
            "the dedicated safety workflow must not skip steps or tolerate failures",
        )

    def test_active_call_parser_rejects_documentary_text(self) -> None:
        rejected = (
            'echo "python3 scripts/test_fake.py"',
            "require_pattern scripts/example.sh 'python3 scripts/test_fake.py' note",
            'printf "%s\\n" "sh scripts/test_fake.sh"',
        )
        for line in rejected:
            self.assertIsNone(PYTHON_TEST_CALL.search(line), line)
            self.assertIsNone(SHELL_TEST_CALL.search(line), line)

        accepted = (
            "python3 scripts/test_real.py",
            "if run_python_script 'scripts/test_real.py' -q >/dev/null 2>&1",
            'if output="$(python3 \'scripts/test_real.py\' 2>&1)"',
        )
        for line in accepted:
            match = PYTHON_TEST_CALL.search(line)
            self.assertIsNotNone(match, line)
            self.assertEqual(match.group("name"), "test_real.py")

    def test_active_owner_requires_failure_propagation(self) -> None:
        accepted = (
            "set -eu\npython3 scripts/test_real.py",
            "exec python3 scripts/test_real.py",
            "if python3 scripts/test_real.py; then\n    :\nelse\n    fail nope\nfi",
        )
        for source in accepted:
            self.assertEqual(
                propagated_test_calls(source, PYTHON_TEST_CALL),
                ("test_real.py",),
                source,
            )

        rejected = (
            "python3 scripts/test_fake.py",
            "set -eu\npython3 scripts/test_fake.py || true",
            "set -eu\npython3 scripts/test_fake.py | tee results.txt",
            "set -eu\npython3 scripts/test_fake.py &",
            "set -eu\nset +e\npython3 scripts/test_fake.py",
            "set -eu\nset +e # temporary\npython3 scripts/test_fake.py",
            "set -eu\nset +e >/dev/null\npython3 scripts/test_fake.py",
            "set -eu\nset +e>/dev/null\npython3 scripts/test_fake.py",
            "set -eu\nset +o errexit>/dev/null\npython3 scripts/test_fake.py",
            "set -eu\npython3 scripts/test_fake.py && echo passed\necho later",
            "set -eu\npython3 scripts/test_fake.py \\\n    && echo passed\necho later",
            "set -e && set +e\npython3 scripts/test_fake.py",
            "if false; then\n    set -e\nfi\npython3 scripts/test_fake.py",
            "if python3 scripts/test_fake.py; then\n    :\nelse\n    true\nfi",
        )
        for source in rejected:
            self.assertEqual(
                propagated_test_calls(source, PYTHON_TEST_CALL), (), source
            )


if __name__ == "__main__":
    unittest.main()
