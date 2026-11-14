# Working on this fork and contributing to Aurora

`origin` is `Minuit-Ferina/Aurora-Viewer`; `upstream` is
`Aurora-Viewer/Aurora-Viewer`. Keep `gh`'s default repository and
`remote.pushDefault` on the fork, and name `--repo` / `-R` on every PR command.
PR numbers are local to a repository. Always push a task branch explicitly
with `git push -u origin HEAD`; never push directly to `main` or `develop`.

The fork's `main` follows Aurora and contains upstream viewer code plus
fork-specific CI, tooling and workflow documentation, starting with fork PR #1. Those commits must not be
included in a contribution to Aurora. Do not reset or force-push `main` to
make the fork look identical to upstream.

`develop` is our usable version: it receives `main` through merge PRs and
our viewer changes through feature PRs. Both branches require PRs and checks.
A worktree is a separate working directory for one branch; it is not another
Git history or a second kind of main.

## Protection of main and develop

The fork requires a pull request, resolved review conversations and these
five checks against the latest target branch (`main` or `develop`):

- `fmt, clippy (Windows)`
- `tests (Windows)`
- `secret scan`
- `clippy (Linux)`
- `clippy, tests, demo (macOS)`

There are no bypass actors; deletion and force pushes are blocked. Required
checks are strict. A PR into the fork must incorporate a newer target branch
and pass its checks again before merging. The existing documentation-only
and platform scope rules still apply. Full Windows tests run on code PRs;
Linux and macOS tests and the Metal demo run when their scope applies.

Squash is the normal method for fork-only changes and feature PRs into
`develop`. **Synchronizations (upstream to main, and main to develop) use
merge commits**, preserving upstream's commit ancestry. The fork therefore
allows both squash and merge, with no linear-history requirement. GitHub
cannot restrict the merge method by PR purpose: the person or agent merging
must choose `-Method merge` for a synchronization. Do not squash or rebase a
sync PR. Aurora's own rules and merge queue remain unchanged.

## Choose the base before starting

Run these commands from the fork's main checkout. The `-SkipCleanup` option
keeps other tasks' existing builds, including a viewer being kept for use.

| Task | Base | PR destination | Merge method |
| --- | --- | --- | --- |
| Fork tooling or CI | `origin/main` | fork `main` | squash |
| Our viewer changes | `origin/develop` | fork `develop` | squash |
| Update our version from main | `origin/develop`, then merge `origin/main` | fork `develop` | merge |
| Synchronize Aurora into the fork | `origin/main`, then merge `upstream/main` | fork `main` | merge |
| Contribute a feature or fix to Aurora | `upstream/main` | Aurora | Aurora's policy |

For fork tooling or CI:

```powershell
./scripts/new-task.ps1 -Name fork-maintenance -Kind chore -BaseRemote origin -SkipCleanup
```

For a change specific to our viewer:

```powershell
./scripts/new-task.ps1 -Name our-feature -Kind feat -BaseRemote origin -BaseBranch develop -SkipCleanup
```

After implementing and testing this change in its worktree, publish it with
an explicit `develop` destination (GitHub's default remains `main`):

```powershell
git push -u origin HEAD
gh pr create --repo Minuit-Ferina/Aurora-Viewer --base develop --head feat/our-feature --title "feat(viewer): describe our change" --body-file .scratch/pr-body.md
```

For an upstream contribution, start clean:

```powershell
./scripts/new-task.ps1 -Name my-feature -Kind feat -BaseRemote upstream -SkipCleanup
```

Each command fetches its chosen remote and branch (`main` by default). A
missing `upstream` fails instead of silently starting from fork `main`. The worktree has its own
`target/` and a branch with no tracking branch until the explicit push.

## Submit a clean contribution to Aurora

Work in `../work/my-feature`, following Aurora's contribution, review,
`TASKS.md`, local checks and human validation requirements. Before opening
the PR, inspect both the commits and GitHub's three-dot diff:

```powershell
git fetch upstream main
git log --oneline upstream/main..HEAD
git diff --stat upstream/main...HEAD
git diff upstream/main...HEAD
git push -u origin HEAD
gh pr create --repo Aurora-Viewer/Aurora-Viewer --base main --head Minuit-Ferina:feat/my-feature --title "feat(viewer): describe the feature" --body-file .scratch/pr-body.md
```

Only the intended contribution and its task documentation should appear.
Do not copy the fork's CI, workflow guide or whole `TASKS.md` onto this
branch. Never merge `origin/main` or `origin/develop` into it, even if a PR into fork `main` or `develop` would
need that to satisfy strict checks. An upstream PR is checked against
**Aurora's** `main` and follows Aurora's update and merge-queue rules.

The clean worktree contains upstream's scripts, so the new fork options
will not be present there. Use the helpers from the fork's main checkout
by absolute path when selecting a PR repository. For example, from the
contribution worktree:

```powershell
$forkMain = Split-Path (git rev-parse --path-format=absolute --git-common-dir).Trim() -Parent
gh pr checks 1234 --repo Aurora-Viewer/Aurora-Viewer
# Only when the PR is ready and you have permission to merge it:
& "$forkMain/scripts/merge-pr.ps1" -Pr 1234 -Repository Aurora-Viewer/Aurora-Viewer
```

Contributors normally let Aurora's maintainers merge their PR. After the
upstream PR is merged, leave the task directory, then clean up explicitly:

```powershell
Set-Location $forkMain
./scripts/end-task.ps1 -Name my-feature -Repository Aurora-Viewer/Aurora-Viewer -Pr 1234
```

The cleanup verifies the PR's repository, head branch and head commit before
removing a clean worktree. PR #1234 in the fork is a different PR.

For screenshots, keep publication on the fork's `captures` branch and use
the fork's main-checkout helper. Its image links name the publishing fork.
Check that a chosen `pr-<number>` directory is not already used for another
PR before publishing; fork and Aurora PR numbers can overlap. If it is already used, attach the images manually in the target PR instead of overwriting it.

If work was already started from fork `main`, create a new contribution
worktree from `upstream/main` and cherry-pick **only the feature commits**,
oldest first. Inspect every commit and the final three-dot diff, resolve
conflicts, and retest. Do not cherry-pick fork CI/setup commits or a broad
range that includes them. Keep the original branch until the export is safe.

If a contribution must be tested or used in the fork before Aurora merges
it, create a separate integration branch from `origin/develop`, cherry-pick the
feature commits there, and open a PR to fork `develop` with `--base develop`. Keep the clean contribution
branch unchanged. Later upstream integration may need conflict resolution
because Aurora can squash or edit the same feature; review the resulting
code instead of automatically accepting either side.

## Synchronize Aurora into main through a fork PR

First, from the main checkout, check whether upstream has commits missing
from the fork:

```powershell
git fetch origin main
git fetch upstream main
git log --oneline origin/main..upstream/main
```

If the list is empty, no sync PR is needed. Otherwise create a fresh task
(use a unique name for each sync), then work inside it:

```powershell
./scripts/new-task.ps1 -Name sync-aurora-2026-11-15 -Kind chore -BaseRemote origin -SkipCleanup
Set-Location ../work/sync-aurora-2026-11-15
$upstreamRevision = (git rev-parse upstream/main).Trim()
$upstreamRevision | Set-Content .scratch/upstream-revision.txt
git merge --no-ff $upstreamRevision -m "Merge Aurora main into the fork"
```

Record that upstream SHA in the PR body; it stays the sync target even if
another task fetches a newer upstream revision while CI runs.

If Git reports conflicts, stop and resolve them before continuing. Review
upstream changes to CI and the fork's documentation carefully: retain full
fork PR testing and this separation of branch bases. Do not blanket-select
ours or theirs. Review `git diff origin/main...HEAD`, run the appropriate
checks (including `scripts/check.ps1` for code changes), then open the PR:

```powershell
git push -u origin HEAD
gh pr create --repo Minuit-Ferina/Aurora-Viewer --base main --head chore/sync-aurora-2026-11-15 --title "chore(sync): merge Aurora main" --body-file .scratch/pr-body.md
gh pr checks 42 --repo Minuit-Ferina/Aurora-Viewer
# After review and all required checks pass:
./scripts/merge-pr.ps1 -Pr 42 -Repository Minuit-Ferina/Aurora-Viewer -Method merge
```

If fork `main` advances before the merge, fetch it, merge `origin/main`
into this sync branch, push, review and wait for the checks again. Never use
an admin bypass or weaken required checks to finish a sync.

After merging, return to the main checkout in a separate command, then:

```powershell
git fetch origin main
git merge --ff-only origin/main
git merge-base --is-ancestor $upstreamRevision origin/main
./scripts/end-task.ps1 -Name sync-aurora-2026-11-15 -Repository Minuit-Ferina/Aurora-Viewer -Pr 42
```

The ancestry check must exit with code 0 for the upstream revision recorded
in the PR (restore `$upstreamRevision` from that SHA if using a new shell).
A later upstream update naturally needs another sync. Do not
use GitHub's one-click sync or `gh repo sync` to bypass the protected PR path.

## Bring main into our version

After an upstream sync, fetch both fork branches and check whether `main`
is already an ancestor of `develop`:

```powershell
git fetch origin main develop
git merge-base --is-ancestor origin/main origin/develop
```

Exit 0 means no integration is needed. Exit 1 means create a fresh task from
`develop`, merge `origin/main`, resolve conflicts and test the combined code:

```powershell
./scripts/new-task.ps1 -Name update-develop-2026-11-15 -Kind chore -BaseRemote origin -BaseBranch develop -SkipCleanup
Set-Location ../work/update-develop-2026-11-15
git merge --no-ff origin/main -m "Merge main into develop"
./scripts/check.ps1
git push -u origin HEAD
gh pr create --repo Minuit-Ferina/Aurora-Viewer --base develop --head chore/update-develop-2026-11-15 --title "chore(sync): update develop from main" --body-file .scratch/pr-body.md
```

Review this PR and wait for all its required checks. Merge with
`merge-pr.ps1 -Repository Minuit-Ferina/Aurora-Viewer -Pr <number> -Method merge`.
If `develop` moves first, merge the latest `origin/develop` into the task
branch and rerun checks. Never merge `develop` back into `main`: our viewer
changes belong in `develop` and in clean contribution branches to Aurora.

To build our current version, use a separate worktree from `origin/develop`
with `-BaseBranch develop -SkipCleanup` and the normal default dev profile.
Keep a working build until its replacement has been checked by the human.
