#!/usr/bin/env bash
# scripts/sync-upstreams.sh
#
# Pull the latest commits from both upstream repositories into their
# respective subdirectories using git subtree --squash.
#
# Usage:
#   ./scripts/sync-upstreams.sh
#
# Each run produces at most two merge commits (one per subtree).
# If there are no new upstream commits, git reports "Subtree is already
# at commit <sha>" and exits cleanly with no new commits.
#
# Conflict resolution:
#   If upstream changed a file you also changed, git will pause and ask
#   you to resolve the conflict exactly like a normal `git merge`.
#   After resolving: git add <file> && git merge --continue
#
# To contribute a local change back upstream:
#   git subtree push --prefix=ironclaw    upstream-ironclaw    my-feature-branch
#   git subtree push --prefix=hermes-agent upstream-hermes     my-feature-branch

set -euo pipefail

# Ensure git-subtree is available (it lives in /usr/lib/git-core/ on some
# systems but is not on PATH; the symlink in /usr/local/libexec/git-core/
# was created during initial setup).
if ! git subtree --help >/dev/null 2>&1; then
  echo "ERROR: git subtree is not available." >&2
  echo "Run: sudo ln -sf /usr/lib/git-core/git-subtree \$(git --exec-path)/git-subtree" >&2
  exit 1
fi

echo "==> Fetching upstream-ironclaw (nearai/ironclaw)..."
git fetch upstream-ironclaw

echo "==> Fetching upstream-hermes (NousResearch/hermes-agent)..."
git fetch upstream-hermes

echo "==> Merging ironclaw/ from upstream main..."
git subtree pull --prefix=ironclaw    upstream-ironclaw  main --squash \
  -m "chore(sync): pull ironclaw upstream $(date -u +%Y-%m-%d)"

echo "==> Merging hermes-agent/ from upstream main..."
git subtree pull --prefix=hermes-agent upstream-hermes   main --squash \
  -m "chore(sync): pull hermes-agent upstream $(date -u +%Y-%m-%d)"

echo ""
echo "==> Done. Both subtrees are up to date."
echo "    Run 'git log --oneline -5' to review the sync commits."
