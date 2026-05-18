# Upstream Sync Strategy: Tracking `ironclaw` and `hermes-agent` Upstreams

**Goal:** Receive updates from the upstream repositories while keeping all local customisations intact.

---

## Chosen Approach: `git subtree`

### Why `git subtree` over alternatives

| Approach | Pros | Cons |
|---|---|---|
| **git subtree** | Single repo, no extra tooling, history stays in one place, contributors need no special setup | Slightly verbose pull/push commands |
| git submodule | Clean separation | Every contributor must run `git submodule update`; detached HEAD confusion; harder CI |
| Manual copy + diff | Simple to understand | Error-prone, no automation, easy to lose changes |

`git subtree` is the right choice here because:
- The workspace is already a single git repo with `ironclaw/` and `hermes-agent/` as plain directories.
- No one needs to learn submodule workflows.
- Upstream merges are a single command.
- Your local commits (new crates, Python bridge files, etc.) are never touched by an upstream pull.

---

## Upstream Repository URLs

| Directory | Upstream URL |
|---|---|
| `ironclaw/` | `https://github.com/nearai/ironclaw` |
| `hermes-agent/` | `https://github.com/NousResearch/hermes-agent` |

---

## One-Time Setup

These steps are run **once** to register the upstream remotes and tell git that the existing directories are subtrees.

```bash
# 1. Add upstream remotes (short names used in all future commands)
git remote add upstream-ironclaw   https://github.com/nearai/ironclaw.git
git remote add upstream-hermes     https://github.com/NousResearch/hermes-agent.git

# 2. Fetch both upstreams (no checkout, just downloads objects)
git fetch upstream-ironclaw
git fetch upstream-hermes

# 3. Register the existing directories as subtrees
#    --squash collapses upstream history into one commit so your log stays clean.
#    Use the branch name that matches what you originally copied from (usually 'main').
git subtree add --prefix=ironclaw   upstream-ironclaw   main --squash
git subtree add --prefix=hermes-agent upstream-hermes   main --squash
```

> **Note on `--squash`:** This is strongly recommended. Without it, the entire upstream commit history (thousands of commits) is merged into your repo's history. With `--squash`, each upstream pull becomes a single merge commit in your log.

> **If the directories already have content** (they do), git subtree will detect that and create a merge commit that reconciles the existing tree with the upstream tree. Any files you added or modified will be preserved — git treats them as local changes on top of the upstream base.

---

## Pulling Upstream Updates (Routine Workflow)

Run these whenever you want to bring in new upstream commits:

```bash
# Fetch latest from both upstreams
git fetch upstream-ironclaw
git fetch upstream-hermes

# Merge upstream changes into ironclaw/
git subtree pull --prefix=ironclaw   upstream-ironclaw   main --squash

# Merge upstream changes into hermes-agent/
git subtree pull --prefix=hermes-agent upstream-hermes   main --squash
```

Each command produces a single merge commit. If there are conflicts (upstream changed a file you also changed), git will pause and ask you to resolve them — exactly like a normal `git merge` conflict.

### Conflict resolution tips

- Conflicts will be in files under `ironclaw/` or `hermes-agent/` only.
- Your custom files (e.g. `ironclaw/crates/ironclaw_hermes_bridge/`, `ironclaw/crates/ironclaw_hdc_dsv/`, `hermes-agent/hermes_cli/improvement_dispatcher.py`) are unlikely to conflict unless upstream added files with the same names.
- After resolving: `git add <file>` then `git merge --continue`.

---

## Pushing Local Changes Back to Upstream (Optional)

If you ever want to contribute a change back to the upstream project, `git subtree push` extracts only the commits that touched the subtree prefix and pushes them to a branch on the upstream remote:

```bash
# Push local ironclaw/ changes to a branch on the upstream remote
git subtree push --prefix=ironclaw upstream-ironclaw my-feature-branch

# Push local hermes-agent/ changes to a branch on the upstream remote
git subtree push --prefix=hermes-agent upstream-hermes my-feature-branch
```

You can then open a PR from `my-feature-branch` on the upstream GitHub repo.

---

## Helper Script: `scripts/sync-upstreams.sh`

A convenience script to fetch and merge both upstreams in one command:

```bash
#!/usr/bin/env bash
# scripts/sync-upstreams.sh
# Usage: ./scripts/sync-upstreams.sh
# Pulls the latest commits from both upstream repositories into their
# respective subdirectories using git subtree --squash.

set -euo pipefail

echo "==> Fetching upstream-ironclaw..."
git fetch upstream-ironclaw

echo "==> Fetching upstream-hermes..."
git fetch upstream-hermes

echo "==> Merging ironclaw/ from upstream..."
git subtree pull --prefix=ironclaw   upstream-ironclaw   main --squash

echo "==> Merging hermes-agent/ from upstream..."
git subtree pull --prefix=hermes-agent upstream-hermes   main --squash

echo "==> Done. Both subtrees are up to date."
```

Make it executable once: `chmod +x scripts/sync-upstreams.sh`

---

## Workflow Diagram

```
Your repo (Ironclaw-W-Self-Learning_Unofficial)
  │
  ├── ironclaw/          ← git subtree prefix
  │     └── (upstream: github.com/nearai/ironclaw, branch: main)
  │
  ├── hermes-agent/      ← git subtree prefix
  │     └── (upstream: github.com/NousResearch/hermes-agent, branch: main)
  │
  ├── plans/             ← your files, never touched by upstream pulls
  └── README.md          ← your files, never touched by upstream pulls

Upstream pull flow:
  upstream-ironclaw/main  ──git subtree pull──►  ironclaw/
  upstream-hermes/main    ──git subtree pull──►  hermes-agent/

Your custom files are preserved because:
  - git subtree only merges changes to files that exist in the upstream
  - Files you added (new crates, bridge code, etc.) are invisible to upstream
  - Files you modified get a standard 3-way merge
```

---

## Important Notes

1. **Your custom crates** (`ironclaw/crates/ironclaw_hermes_bridge/`, `ironclaw/crates/ironclaw_hdc_dsv/`) and **Python files** (`hermes-agent/hermes_cli/improvement_dispatcher.py`, etc.) are safe — upstream has no knowledge of them and will never delete or overwrite them.

2. **`ironclaw/Cargo.toml` workspace members list** — you added `ironclaw_hermes_bridge` and `ironclaw_hdc_dsv` to the `members` array. If upstream also modifies `Cargo.toml`, you will get a merge conflict on that line. Resolution: keep both your additions and the upstream changes.

3. **`hermes-agent/hermes_cli/main.py` or `conversation_loop.py`** — if you patched these to call `improvement_dispatcher`, upstream changes to the same files will produce conflicts. Resolution: re-apply your patch on top of the upstream version.

4. **Branch strategy recommendation:** Do your upstream syncs on a dedicated branch (e.g. `upstream-sync`) and then merge that into `main`. This keeps your main branch clean and gives you a chance to review upstream changes before they land.

---

## Step-by-Step Execution Order

1. `git remote add upstream-ironclaw https://github.com/nearai/ironclaw.git`
2. `git remote add upstream-hermes https://github.com/NousResearch/hermes-agent.git`
3. `git fetch upstream-ironclaw`
4. `git fetch upstream-hermes`
5. `git subtree add --prefix=ironclaw upstream-ironclaw main --squash`
6. `git subtree add --prefix=hermes-agent upstream-hermes main --squash`
7. `mkdir -p scripts && cat > scripts/sync-upstreams.sh` (paste script above)
8. `chmod +x scripts/sync-upstreams.sh`
9. Commit everything: `git add scripts/sync-upstreams.sh && git commit -m "chore: add upstream sync script"`
10. Future syncs: `./scripts/sync-upstreams.sh`
