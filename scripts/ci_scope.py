#!/usr/bin/env python3
"""Dynamic CI scope resolver — one file shared by kymido and ferrite.

Answers "what does this diff actually have to build and test?", and prints
the answer in a form a workflow step can consume. It replaces the inline
python that used to live inside kymido's `.github/workflows/ci.yml`, and the
scope half of ferrite's `scripts/ci-affected.sh`.

Design notes
------------
* `cargo metadata --format-version 1 --no-deps`, never full metadata. The
  reverse-dependency closure only needs workspace-internal edges, and every
  one of them is a `path` dependency — so the resolver needs no registry, no
  git fetch and no credentials. Full metadata made this step the step most
  likely to fail: 12 of kymido's 39 failed runs died here because the
  private `ui-kit` git dependency refused to authenticate. Verified offline:
  `CARGO_NET_OFFLINE=true cargo metadata --no-deps` exits 0.
* "Not owned by any crate" means SKIP, not FULL. The old rule fell back to
  the full workspace for a README edit — 62% of kymido's main commits hit
  that fallback, 25% of them on docs alone. Anything that cannot break a test
  should cost nothing.
* The only orphan that still forces FULL is an orphan `.rs` / `Cargo.toml`:
  that means a crate directory the workspace does not know about, which is a
  resolver bug rather than a docs change, and guessing small there silently
  drops coverage.

Repo-specific mapping travels in as flags rather than as edits, so the file
byte-identical in both repositories::

    # kymido: the UI contract anchors live outside any crate dir
    scripts/ci_scope.py --seed '.githooks/spec/**=web'

    # ferrite: sqlx::migrate! globs db/migrations at compile time, and the
    # frontend crates are wasm32 targets rather than native test hosts
    scripts/ci_scope.py --seed 'db/migrations/**=db-bootstrap'
                        --wasm-dir crates/web --wasm-dir apps/admin-web
                        --wasm-dir apps/tavern-web

Execution is deliberately *not* part of this script: it only answers the
question, then a workflow consumes it. `--format github` writes eight keys to
$GITHUB_OUTPUT — `skip` / `full` / `names` / `seed` / `reason`, plus the three
execution buckets `test` (integration test targets exist -> `cargo test -p`),
`wasm` (dir under a `--wasm-dir` -> `cargo check --target wasm32-unknown-unknown`),
and `check` (neither -> plain `cargo check`). A package can sit in both `test`
and `wasm`: ferrite tests web packages natively *and* wasm-checks them, which
is exactly what its old `ci-affected.sh` did. Kymido reads only the first five
keys — the buckets stay empty of meaning there because it passes no
`--wasm-dir`.

Usage
-----
    ci_scope.py [--base REF] [--format github|text|json] [--explain]
                [--seed 'glob=package'] [--ignore 'glob'] [--also 'pkg=pkg']
                [--wasm-dir DIR] [--no-worktree]

Exit codes: 0 resolved, 1 could not resolve a scope (workflow should fail
loudly rather than silently test nothing).
"""

from __future__ import annotations

import argparse
import fnmatch
import json
import os
import re
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import TextIO

import tomllib

# `#[test]` / `#[tokio::test]` plus any other attributes glued to the item,
# then the fn name. Only used for the --explain report, never for control flow.
TEST_FN_RE = re.compile(
    r"#\[(?:tokio|async_std|smol)?::?test[^\]]*\]\s*"
    r"(?:#\[[^\]]*\]\s*)*"
    r"(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)"
)


def git(*args: str, cwd: Path | None = None) -> str:
    """Run git, return stdout. Raises SystemExit(1) with stderr on failure."""
    proc = subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, check=False
    )
    if proc.returncode != 0:
        raise ScopeError(f"git {' '.join(args)}: {proc.stderr.strip()}")
    return proc.stdout


def git_ok(*args: str, cwd: Path | None = None) -> str | None:
    proc = subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, check=False
    )
    return proc.stdout if proc.returncode == 0 else None


class ScopeError(RuntimeError):
    """A scope could not be computed — the caller should fail, not guess."""


def _blob(root: Path, rev: str, path: str) -> str:
    """File content at a revision. Missing blob (new/deleted file) is `""`."""
    return git_ok("show", f"{rev}:{path}", cwd=root) or ""


# --------------------------------------------------------------------------- #
# diff
# --------------------------------------------------------------------------- #
def resolve_base(explicit: str | None, root: Path) -> str:
    """Pick the diff base. In CI this follows the event; locally it walks
    common branch names so `ci_scope.py` needs no flags."""
    if explicit:
        return explicit

    event = os.environ.get("GITHUB_EVENT_NAME", "")
    if event == "pull_request":
        base_ref = os.environ.get("GITHUB_BASE_REF", "")
        if base_ref:
            return f"origin/{base_ref}"
    if event == "push":
        before = os.environ.get("GITHUB_EVENT_BEFORE", "")
        # First push of a branch / new repo: zero SHA, nothing to diff against.
        if before and before != "0" * 40:
            return before
        return "HEAD^"

    for candidate in ("origin/main", "main", "HEAD~1"):
        if git_ok("rev-parse", "--verify", candidate, cwd=root) is not None:
            return candidate
    raise ScopeError("no diff base: no --base, no CI event, no main branch")


def changed_files(base: str, root: Path, with_worktree: bool) -> tuple[str, list[str]]:
    """Merge-base diff of `base`...HEAD. merge-base (rather than a straight
    `base..HEAD`) keeps a force-pushed base from widening the diff."""
    if git_ok("rev-parse", "--verify", base, cwd=root) is None:
        raise ScopeError(f"diff base '{base}' does not exist")
    merge_base = git("merge-base", base, "HEAD", cwd=root).strip()
    diff = git("diff", "--name-only", merge_base, "HEAD", cwd=root)

    files: list[str] = [line.strip() for line in diff.splitlines() if line.strip()]

    # Outside CI also fold in the working tree, so a local run reports what
    # the agent is actually about to commit. Untracked files are deliberately
    # excluded: they are build output (target/, *.log, todo/ drafts) and would
    # push every local run to FULL.
    if with_worktree:
        for args in (["diff", "--name-only", "HEAD"], ["diff", "--name-only", "--cached", "HEAD"]):
            for line in git(*args, cwd=root).splitlines():
                line = line.strip()
                if line and line not in files:
                    files.append(line)

    return merge_base, files


# --------------------------------------------------------------------------- #
# workspace graph
# --------------------------------------------------------------------------- #
class Workspace:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.members: list[str] = []
        self.packages: dict[str, dict] = {}   # name -> package
        self.dir_of: dict[str, str] = {}      # package name -> repo-relative dir
        self.dependents: dict[str, set[str]] = {}  # package -> packages depending on it
        self._load()

    def _load(self) -> None:
        proc = subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--no-deps"],
            cwd=self.root, capture_output=True, text=True, check=False,
        )
        if proc.returncode != 0:
            # --no-deps is offline-safe; reaching here means the workspace
            # manifest itself is broken, which is worth failing on.
            raise ScopeError(f"cargo metadata: {proc.stderr.strip()}")
        meta = json.loads(proc.stdout)

        member_ids = set(meta["workspace_members"])
        prefix = self.root.as_posix().rstrip("/") + "/"
        for pkg in meta["packages"]:
            if pkg["id"] not in member_ids:
                continue
            self.packages[pkg["name"]] = pkg
            self.dir_of[pkg["name"]] = (
                Path(pkg["manifest_path"]).parent.as_posix().removeprefix(prefix)
            )
            self.dependents[pkg["name"]] = set()
        self.members = sorted(self.packages)

        # Workspace-internal edges from the `path` key alone: no `resolve`
        # graph needed, which is what keeps this offline.
        for pkg in meta["packages"]:
            if pkg["id"] not in member_ids:
                continue
            for dep in pkg.get("dependencies", []):
                if dep.get("path") is None:
                    continue
                dep_name = dep.get("package", dep["name"])
                if dep_name in self.dependents:
                    self.dependents[dep_name].add(pkg["name"])

    def owner_of(self, path: str) -> str | None:
        """Longest manifest-dir prefix wins, so `crates/foo/` beats `crates/`."""
        best, best_len = None, -1
        for name, directory in self.dir_of.items():
            if (path == directory or path.startswith(directory + "/")) and len(directory) > best_len:
                best, best_len = name, len(directory)
        return best

    def closure(self, seeds: set[str]) -> set[str]:
        """Reverse-dependency closure: a change to X has to keep passing in
        everything that uses X."""
        seen, stack = set(seeds), list(seeds)
        while stack:
            for parent in self.dependents.get(stack.pop(), ()):
                if parent not in seen:
                    seen.add(parent)
                    stack.append(parent)
        return seen

    def test_targets(self, names: set[str]) -> dict[str, list[str]]:
        """Integration-test source files per affected package."""
        out: dict[str, list[str]] = {}
        for name in sorted(names):
            pkg = self.packages.get(name)
            if not pkg:
                continue
            files = sorted(
                Path(t["src_path"]).relative_to(self.root).as_posix()
                for t in pkg.get("targets", [])
                if "test" in t["kind"]
            )
            if files:
                out[name] = files
        return out

    def manifest_deps(self) -> dict[str, set[str]]:
        """package -> direct dependency names (workspace-internal and external
        alike), used to map a `[workspace.dependencies]` edit to its consumers."""
        return {
            name: {d["name"] for d in pkg.get("dependencies", [])}
            for name, pkg in self.packages.items()
        }


# --------------------------------------------------------------------------- #
# root manifest analysis
# --------------------------------------------------------------------------- #
@dataclass
class ManifestVerdict:
    """What a root-manifest change actually means for the build graph."""

    mode: str                 # "full" | "deps" | "cosmetic"
    touched_deps: set[str]    # [workspace.dependencies] keys added/changed/removed
    reason: str = ""


def _parse_toml(text: str) -> dict | None:
    try:
        return tomllib.loads(text)
    except (tomllib.TOMLDecodeError, ValueError):
        return None


def analyse_root_manifest(before: str, after: str) -> ManifestVerdict:
    """Compare the two versions of the root manifest instead of pattern-matching
    its diff.

    The diff form is fragile in a way this repository already paid for: with
    `-U10000` the table header is a *context* line, not a `+` line, so a rule
    that only looks at added lines never learns it is inside
    `[workspace.dependencies]` and reports "unattributable" (FULL) for a plain
    `zstd = "0.13"` addition. Parsing both blobs sidesteps line provenance
    entirely.

    Verdicts:
      * anything outside `[workspace.dependencies]` moved (members, resolver,
        package, profile, patch, metadata) -> the whole graph moves -> FULL
      * only dependency entries changed -> seed their consumers, keep the run
        narrow
      * pure comment/whitespace churn -> cosmetic, no extra seeds
    """
    before_toml, after_toml = _parse_toml(before), _parse_toml(after)
    if before_toml is None or after_toml is None:
        return ManifestVerdict("full", set(), "root manifest is not parseable TOML")

    ws_before = dict(before_toml.get("workspace", {}))
    ws_after = dict(after_toml.get("workspace", {}))
    deps_before = ws_before.pop("dependencies", {})
    deps_after = ws_after.pop("dependencies", {})

    if ws_before != ws_after:
        moved = sorted(
            k for k in set(ws_before) | set(ws_after)
            if ws_before.get(k) != ws_after.get(k)
        )
        return ManifestVerdict("full", set(), f"workspace-level keys changed: {', '.join(moved)}")

    rest_before = {k: v for k, v in before_toml.items() if k != "workspace"}
    rest_after = {k: v for k, v in after_toml.items() if k != "workspace"}
    if rest_before != rest_after:
        moved = sorted(
            k for k in set(rest_before) | set(rest_after)
            if rest_before.get(k) != rest_after.get(k)
        )
        # `[patch]` rewrites which crate a dependency resolves to, `[profile]`
        # changes how every crate is compiled — neither is a per-crate seed.
        return ManifestVerdict("full", set(), f"top-level keys changed: {', '.join(moved)}")

    touched = {
        k for k in set(deps_before) | set(deps_after)
        if deps_before.get(k) != deps_after.get(k)
    }
    if not touched:
        return ManifestVerdict("cosmetic", set(), "root manifest changed cosmetically")
    return ManifestVerdict("deps", touched, f"workspace deps changed: {', '.join(sorted(touched))}")


# --------------------------------------------------------------------------- #
# scope
# --------------------------------------------------------------------------- #
class Scope:
    def __init__(self, mode: str, reason: str) -> None:
        self.mode = mode            # "full" | "dynamic" | "skip"
        self.reason = reason
        self.seeds: list[str] = []
        self.packages: list[str] = []
        self.owners: dict[str, str] = {}   # changed file -> owning package
        self.ignored: list[str] = []
        self.base = ""
        self.merge_base = ""
        self.changed: list[str] = []
        self.ws: Workspace | None = None   # cached: emit both formats from one metadata call
        # Execution buckets (filled by split_buckets): what to `cargo test`,
        # what to `cargo check`, what to wasm-check. Empty lists when no
        # --wasm-dir was given — a consumer then just uses `names`.
        self.wasm_dirs: list[str] = []
        self.test_pkgs: list[str] = []
        self.check_pkgs: list[str] = []
        self.wasm_pkgs: list[str] = []


def split_buckets(scope: Scope, wasm_dirs: list[str]) -> None:
    """Divide the in-scope packages into the three buckets CI executes.

    Mirrors ferrite's old `ci-affected.sh` exactly:
      * has integration test targets -> `cargo test -p` (even when web)
      * dir under a --wasm-dir      -> `cargo check --target wasm32-unknown-unknown`
        (independent of the test bucket: a web package with tests is both
        tested natively and wasm-checked)
      * neither                     -> plain `cargo check`

    Without --wasm-dir the wasm bucket stays empty and every non-test package
    lands in `check` — kymido ignores these keys and drives everything from
    `names`, so the split costs it nothing.
    """
    scope.wasm_dirs = list(wasm_dirs)
    ws = scope.ws
    if ws is None:
        return

    # A typo'd dir would silently push wasm-only crates into a native
    # `cargo check` and the mistake would surface as a confusing compile
    # error (or worse, not surface at all). Fail like --seed/--also do.
    for wanted in wasm_dirs:
        if not any(
            directory == wanted or directory.startswith(wanted.rstrip("/") + "/")
            for directory in ws.dir_of.values()
        ):
            raise ScopeError(f"--wasm-dir '{wanted}' matches no workspace package")

    has_tests = set(ws.test_targets(set(scope.packages)))
    for name in scope.packages:
        directory = ws.dir_of[name]
        under_wasm = any(
            directory == wanted or directory.startswith(wanted.rstrip("/") + "/")
            for wanted in wasm_dirs
        )
        if under_wasm:
            scope.wasm_pkgs.append(name)
        if name in has_tests:
            scope.test_pkgs.append(name)
        elif not under_wasm:
            scope.check_pkgs.append(name)


def _match(path: str, pattern: str) -> bool:
    """`glob/**` is a prefix match (fnmatch's `*` also crosses `/`, but the
    explicit form keeps `--seed '.githooks/spec/**=web'` readable)."""
    if pattern.endswith("**"):
        return path.startswith(pattern[:-2])
    return fnmatch.fnmatch(path, pattern)


def _parse_multi(values: list[str], flag: str) -> list[tuple[str, str]]:
    out = []
    for value in values:
        if "=" not in value:
            raise ScopeError(f"{flag} expects 'glob=package', got '{value}'")
        pattern, target = value.split("=", 1)
        out.append((pattern, target))
    return out


def _parse_also(values: list[str]) -> list[tuple[str, str]]:
    out = []
    for value in values:
        if "=" not in value:
            raise ScopeError(f"--also expects 'package=package', got '{value}'")
        trigger, extra = value.split("=", 1)
        out.append((trigger, extra))
    return out


def resolve(
    root: Path,
    base: str | None,
    seeds: list[tuple[str, str]],
    ignores: list[str],
    with_worktree: bool,
    also: list[tuple[str, str]] | None = None,
) -> Scope:
    ws = Workspace(root)
    merge_base, files = changed_files(resolve_base(base, root), root, with_worktree)

    if not files:
        scope = Scope("skip", "empty diff")
        scope.base, scope.merge_base, scope.ws = base or "", merge_base, ws
        return scope

    scope = Scope("dynamic", "")
    scope.base, scope.merge_base, scope.changed = base or "", merge_base, files
    scope.ws = ws

    # --- paths that can move the whole graph ----------------------------- #
    for path in files:
        if path in ("rust-toolchain", "rust-toolchain.toml"):
            scope.mode, scope.reason = "full", "toolchain file changed"
            scope.packages = list(ws.members)
            return scope

    root_manifest_changed = "Cargo.toml" in files
    any_manifest_changed = any(
        path == "Cargo.toml" or path.endswith("/Cargo.toml") for path in files
    )

    # Read both blobs rather than the diff text: with -U10000 the table header
    # is context, not `+`, so a line-oriented rule never learns it is inside
    # `[workspace.dependencies]` and calls a plain `zstd = "0.13"` addition
    # unattributable.
    touched_deps: set[str] = set()
    if root_manifest_changed:
        verdict = analyse_root_manifest(
            _blob(root, merge_base, "Cargo.toml"), _blob(root, "HEAD", "Cargo.toml")
        )
        if verdict.mode == "full":
            scope.mode, scope.reason = "full", f"root manifest: {verdict.reason}"
            scope.packages = list(ws.members)
            return scope
        touched_deps = verdict.touched_deps

    # A lockfile no manifest explains is a bare `cargo update`: versions can
    # move for packages nothing in the graph names, so the safe answer stays
    # the whole graph.
    if "Cargo.lock" in files and not any_manifest_changed:
        scope.mode, scope.reason = "full", "Cargo.lock re-resolved without a manifest change"
        scope.packages = list(ws.members)
        return scope

    # --- classify every changed path ------------------------------------- #
    consumers = ws.manifest_deps()
    direct: set[str] = set()
    orphans: list[str] = []

    for path in files:
        if path.startswith(".github/"):
            # Workflow YAML: GitHub validates it by running it, and the run
            # *is* the new configuration proving itself. Upgrading this to
            # FULL made every CI tweak pay the whole workspace (kymido: 14 of
            # 60 commits).
            scope.ignored.append(path)
            continue
        if path in ("Cargo.lock", "Cargo.toml"):
            continue  # handled above

        matched = False
        for pattern, target in seeds:
            if _match(path, pattern):
                if target not in ws.packages:
                    raise ScopeError(f"--seed target '{target}' is not a workspace member")
                direct.add(target)
                scope.owners[path] = f"{target} (seed rule)"
                matched = True
                break
        if matched:
            continue
        if any(_match(path, pattern) for pattern in ignores):
            scope.ignored.append(path)
            continue

        owner = ws.owner_of(path)
        if owner is not None:
            direct.add(owner)
            scope.owners[path] = owner
        else:
            orphans.append(path)

    # --- root manifest dependency entries -> their consumers --------------- #
    if touched_deps:
        for dep_name in sorted(touched_deps):
            for pkg, deps in consumers.items():
                if dep_name in deps:
                    direct.add(pkg)
        if direct:
            scope.owners["Cargo.toml"] = f"consumers of {', '.join(sorted(touched_deps))}"

    # --- orphan handling --------------------------------------------------- #
    # An orphan Rust source or manifest means the resolver's idea of the
    # workspace is stale (a crate dir nobody registered). Falling back to SKIP
    # there would drop coverage silently, so it falls back to FULL instead.
    code_orphans = [p for p in orphans if p.endswith((".rs", "Cargo.toml"))]
    if code_orphans:
        scope.mode = "full"
        scope.reason = f"Rust source outside every crate dir: {', '.join(sorted(code_orphans)[:3])}"
        scope.packages = list(ws.members)
        return scope

    if not direct:
        scope.mode = "skip"
        scope.reason = (
            "no workspace crate owns the changed paths (docs/config only): "
            + ", ".join(sorted(orphans)[:5])
            if orphans
            else "empty diff after filtering"
        )
        scope.ignored.extend(orphans)
        return scope

    scope.seeds = sorted(direct)
    scope.packages = sorted(ws.closure(direct))
    reason = (
        f"{len(scope.seeds)} direct hit(s) expanded to {len(scope.packages)} "
        f"of {len(ws.members)} crates"
    )

    # An e2e test that execs a *dependency's* example binary (daemon's
    # subagent e2e runs `mock_acp_server`, an example of `subagent`) cannot get
    # it from the reverse-dependency closure: cargo only builds examples of the
    # packages named with `-p`. Pull the extra package in whenever its trigger
    # is in scope (run 36377398489: "spawn failed: No such file or directory").
    pulled = []
    for trigger, extra in also or ():
        if trigger in scope.packages and extra in ws.packages and extra not in scope.packages:
            scope.packages = sorted(set(scope.packages) | {extra})
            pulled.append(extra)
    if pulled:
        reason += f" (+{', '.join(pulled)}: example binary needed at runtime)"

    scope.reason = reason
    return scope


# --------------------------------------------------------------------------- #
# output
# --------------------------------------------------------------------------- #
def inline_test_functions(root: Path, files: list[str]) -> dict[str, list[str]]:
    """`#[test] fn ...` names inside the changed sources — the narrowest
    answer to "which test functions does this diff touch?"."""
    out: dict[str, list[str]] = {}
    for rel in files:
        if not rel.endswith(".rs"):
            continue
        path = root / rel
        if not path.is_file():
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        names = TEST_FN_RE.findall(text)
        if names:
            out[rel] = sorted(set(names))
    return out


def emit_github(scope: Scope) -> None:
    """Key=value pairs for `$GITHUB_OUTPUT`.

    `names` is always the concrete package list (all members when FULL, empty
    when skipping), so a step can build one `-p` array and an `if:` condition
    can token-match a single crate without a separate `__ALL__` sentinel.
    `test` / `check` / `wasm` are the execution buckets (see split_buckets);
    they partition `names` except that wasm ∩ test is allowed to overlap.
    """
    lines = [
        f"skip={'true' if scope.mode == 'skip' else 'false'}",
        f"full={'true' if scope.mode == 'full' else 'false'}",
        f"names={','.join(scope.packages)}",
        f"seed={','.join(scope.seeds)}",
        f"reason={scope.reason}".replace("\n", " "),
        f"test={','.join(scope.test_pkgs)}",
        f"check={','.join(scope.check_pkgs)}",
        f"wasm={','.join(scope.wasm_pkgs)}",
    ]
    payload = "\n".join(lines) + "\n"
    target = os.environ.get("GITHUB_OUTPUT")
    if target:
        with open(target, "a", encoding="utf-8") as fh:
            fh.write(payload)
    else:
        sys.stdout.write(payload)


def emit_text(scope: Scope, explain: bool, out: TextIO | None = None) -> None:
    """Human report. `out` lets `--format github` divert this to the step log
    while stdout stays a pure `key=value` stream (the script's contract when
    `GITHUB_OUTPUT` is unset)."""
    fh = out or sys.stdout

    def p(msg: str = "") -> None:
        print(msg, file=fh)

    ws = scope.ws
    p(f"Scope  : {scope.mode.upper()}")
    p(f"Reason : {scope.reason}")
    if scope.base:
        p(f"Base   : {scope.base} (merge-base {scope.merge_base[:12]})")
    p(f"Changed: {len(scope.changed)} file(s)")

    if scope.mode == "skip":
        for path in scope.ignored:
            p(f"  ignored  {path}")
        return

    p(f"Seed   : {len(scope.seeds)} -> {', '.join(scope.seeds) or '(all)'}")
    p(f"Closure: {len(scope.packages)} crates")
    p(f"  {', '.join(scope.packages)}")

    # With --wasm-dir the consumer executes three different commands, so the
    # report names them per bucket; without it (kymido) every package is
    # driven by one `cargo test -p` list from `names`.
    if scope.wasm_dirs:
        if scope.test_pkgs:
            p(f"test   : cargo test {' '.join('-p ' + n for n in scope.test_pkgs)}")
        if scope.check_pkgs:
            p(f"check  : cargo check {' '.join('-p ' + n for n in scope.check_pkgs)}")
        if scope.wasm_pkgs:
            p(
                "wasm   : cargo check --target wasm32-unknown-unknown "
                + " ".join("-p " + n for n in scope.wasm_pkgs)
            )
    else:
        p(f"cargo  : cargo test {' '.join('-p ' + pkg for pkg in scope.packages)}")

    if not explain or ws is None:
        return

    p("\n-- test targets --")
    targets = ws.test_targets(set(scope.packages))
    total = 0
    for pkg, files in targets.items():
        total += len(files)
        p(f"  {pkg}:")
        for f in files:
            p(f"    {f}")
    p(f"  ({total} test binaries)")

    p("\n-- #[test] fns inside the changed files --")
    fns = inline_test_functions(ws.root, scope.changed)
    if not fns:
        p("  (none)")
    for path in sorted(fns):
        p(f"  {path}")
        for name in fns[path]:
            p(f"    fn {name}")

    if scope.owners:
        p("\n-- file -> owner --")
        for path in sorted(scope.owners):
            p(f"  {scope.owners[path]:<24} {path}")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(
        prog="ci_scope.py",
        description="Resolve which crates / test targets a diff has to exercise.",
    )
    parser.add_argument("--base", help="diff base ref (default: derived from the CI event, else origin/main)")
    parser.add_argument("--format", choices=("github", "text", "json"), default="text")
    parser.add_argument("--explain", action="store_true",
                        help="also list test target files and #[test] fns in changed files")
    parser.add_argument("--seed", action="append", default=[], metavar="GLOB=PKG",
                        help="extra seed: paths matching GLOB pull in workspace package PKG (repeatable)")
    parser.add_argument("--ignore", action="append", default=[], metavar="GLOB",
                        help="paths matching GLOB never seed anything (repeatable)")
    parser.add_argument("--also", action="append", default=[], metavar="PKG=PKG",
                        help="if the first package is in scope, pull in the second "
                             "(e.g. daemon=subagent: its example binary is exec'd "
                             "by daemon's e2e tests) (repeatable)")
    parser.add_argument("--wasm-dir", action="append", default=[], metavar="DIR",
                        help="packages whose manifest dir is DIR (or under it) also get "
                             "`cargo check --target wasm32-unknown-unknown` instead of "
                             "being native-only (e.g. crates/web) (repeatable)")
    parser.add_argument("--no-worktree", action="store_true",
                        help="ignore uncommitted/staged changes (always on in GitHub Actions)")
    parser.add_argument("--root", default=None, help="repository root (default: git toplevel)")
    args = parser.parse_args(argv)

    root = Path(args.root).resolve() if args.root else Path(git("rev-parse", "--show-toplevel").strip())
    in_ci = os.environ.get("GITHUB_ACTIONS") == "true"
    with_worktree = not args.no_worktree and not in_ci

    try:
        scope = resolve(
            root,
            args.base,
            _parse_multi(args.seed, "--seed"),
            args.ignore,
            with_worktree,
            also=_parse_also(args.also),
        )
        # A typo in a flag has to fail the job, not silently narrow the scope.
        for trigger, extra in _parse_also(args.also):
            known = scope.ws.packages if scope.ws else {}
            for name in (trigger, extra):
                if name not in known:
                    raise ScopeError(f"--also '{trigger}={extra}': '{name}' is not a workspace member")
        split_buckets(scope, args.wasm_dir)
    except ScopeError as exc:
        print(f"ci_scope: {exc}", file=sys.stderr)
        return 1

    if args.format == "github":
        # Machine keys go to $GITHUB_OUTPUT (stdout only as a fallback when
        # that file is unset), so the human report picks whichever stream the
        # payload is not using.
        if os.environ.get("GITHUB_OUTPUT"):
            emit_text(scope, args.explain, out=sys.stdout)
        else:
            emit_text(scope, args.explain, out=sys.stderr)
        emit_github(scope)
    elif args.format == "json":
        print(json.dumps({
            "mode": scope.mode,
            "reason": scope.reason,
            "base": scope.base,
            "merge_base": scope.merge_base,
            "changed": scope.changed,
            "seed": scope.seeds,
            "packages": scope.packages,
            "buckets": {
                "test": scope.test_pkgs,
                "check": scope.check_pkgs,
                "wasm": scope.wasm_pkgs,
            },
            "owners": scope.owners,
            "ignored": scope.ignored,
        }, indent=2))
    else:
        emit_text(scope, args.explain)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
