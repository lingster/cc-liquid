"""Utilities to install Click shell completion for the ``cc-liquid`` CLI.

This module generates shell completion scripts for Bash, Zsh, and Fish by
invoking the current executable in Click's completion mode and writes them to
standard user locations. For Bash/Zsh it also appends a ``source`` line to the
user's shell rc file idempotently.

Design notes:
- We keep this module UI-free and return structured messages so the CLI layer
  can print as desired. All file operations are idempotent to avoid duplicate
  rc entries.
"""

from __future__ import annotations

import os
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Literal


ShellName = Literal["bash", "zsh", "fish"]


@dataclass
class InstallResult:
    shell: ShellName
    script_path: Path
    rc_path: Path | None
    rc_line_added: bool
    script_written: bool


def detect_shell_from_env(env: dict[str, str] | None = None) -> ShellName | None:
    """Detect the user's shell from the SHELL environment variable.

    Returns the lowercase shell name (``bash``, ``zsh``, or ``fish``) or ``None``
    when detection fails.
    """
    e = env or os.environ
    shell_path = e.get("SHELL", "").strip()
    if not shell_path:
        return None
    name = Path(shell_path).name.lower()
    if name in {"bash", "zsh", "fish"}:
        return name
    return None


def _compute_env_var_name_for_prog(prog_name: str) -> str:
    """Compute the Click completion env var name for a given program name.

    Example: ``cc-liquid`` -> ``_CC_LIQUID_COMPLETE``.
    """
    safe = prog_name.replace("-", "_").upper()
    return f"_{safe}_COMPLETE"


def _resolve_executable_to_invoke(prog_name: str) -> str:
    """Resolve which executable name to invoke to generate completion.

    Prefers an on-PATH executable matching ``prog_name``; otherwise returns
    ``prog_name`` so the caller still attempts to run it.
    """
    resolved = shutil.which(prog_name)
    return resolved or prog_name


def generate_completion_source(prog_name: str, shell: ShellName) -> str:
    """Generate the completion script text for ``prog_name`` and ``shell``.

    This invokes the current CLI with the appropriate environment variable set
    to ``{shell}_source`` and captures stdout.
    """
    env_var = _compute_env_var_name_for_prog(prog_name)
    env = os.environ.copy()
    env[env_var] = f"{shell}_source"

    exe = _resolve_executable_to_invoke(prog_name)
    try:
        result = subprocess.run(
            [exe],
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            env=env,
        )
    except FileNotFoundError as exc:
        raise RuntimeError(
            f"Could not find executable '{prog_name}' on PATH. Is it installed?"
        ) from exc
    except subprocess.CalledProcessError as exc:
        raise RuntimeError(
            "Failed to generate completion source. "
            f"Return code {exc.returncode}. Stderr: {exc.stderr.strip()}"
        ) from exc

    return result.stdout


def _paths_for_shell(shell: ShellName) -> tuple[Path, Path | None, str | None]:
    """Return (script_path, rc_path, rc_line) for the given shell.

    - For Bash: script at ``~/.cc-liquid-complete.bash``, source line appended to ``~/.bashrc``.
    - For Zsh: script at ``~/.cc-liquid-complete.zsh``, source line appended to ``~/.zshrc``.
    - For Fish: script at ``~/.config/fish/completions/cc-liquid.fish``, no rc line.
    """
    home = Path.home()
    if shell == "bash":
        script = home / ".cc-liquid-complete.bash"
        rc = home / ".bashrc"
        rc_line = f". {script}"
        return script, rc, rc_line
    if shell == "zsh":
        script = home / ".cc-liquid-complete.zsh"
        rc = home / ".zshrc"
        rc_line = f". {script}"
        return script, rc, rc_line
    # fish
    script = home / ".config" / "fish" / "completions" / "cc-liquid.fish"
    return script, None, None


def _write_text_if_changed(path: Path, content: str) -> bool:
    """Write ``content`` to ``path`` if it differs. Returns True if written."""
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        try:
            current = path.read_text(encoding="utf-8")
        except Exception:
            current = ""
        if current == content:
            return False
    path.write_text(content, encoding="utf-8")
    return True


def _append_line_idempotent(path: Path, line: str) -> bool:
    """Append ``line`` to ``path`` if not already present. Returns True if added."""
    path.parent.mkdir(parents=True, exist_ok=True)
    existing = path.read_text(encoding="utf-8") if path.exists() else ""
    if line in existing:
        return False
    with path.open("a", encoding="utf-8") as f:
        if existing and not existing.endswith("\n"):
            f.write("\n")
        f.write(line + "\n")
    return True


def install_completion(prog_name: str, shell: ShellName) -> InstallResult:
    """Generate and install completion for ``prog_name`` and the given shell.

    Returns an ``InstallResult`` describing what changed.
    """
    script_text = generate_completion_source(prog_name, shell)
    script_path, rc_path, rc_line = _paths_for_shell(shell)

    wrote_script = _write_text_if_changed(script_path, script_text)

    added_rc = False
    if rc_path is not None and rc_line is not None:
        added_rc = _append_line_idempotent(rc_path, rc_line)

    return InstallResult(
        shell=shell,
        script_path=script_path,
        rc_path=rc_path,
        rc_line_added=added_rc,
        script_written=wrote_script,
    )
