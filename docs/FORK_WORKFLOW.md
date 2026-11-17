# Working on this fork and contributing to Aurora

`origin` is `Minuit-Ferina/Aurora-Viewer`; `upstream` is
`Aurora-Viewer/Aurora-Viewer`. Keep `gh`'s default repository and
`remote.pushDefault` on the fork, and name `--repo` / `-R` on every PR command.
PR numbers are local to a repository. Always push a task branch explicitly
with `git push -u origin HEAD`; never push directly to `main` or `develop`.

The fork's `main` holds the earlier upstream snapshot and the initial
fork CI and workflow setup. All new fork PRs target `develop`, including
tooling, viewer changes and synchronization with stable Aurora releases.
`develop` is also the base for the fork's releases. A worktree is a
separate working directory for a branch.

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
`develop`. **Synchronization PRs from an upstream release tag into
`develop` use merge commits**, preserving upstream's commit ancestry. The fork therefore
allows both squash and merge, with no linear-history requirement. GitHub
cannot restrict the merge method by PR purpose: the person or agent merging
must choose `-Method merge` for a synchronization. Do not squash or rebase a
sync PR. Aurora's own rules and merge queue remain unchanged.

## Choose the base before starting

Run these commands from the fork's main checkout. The `-SkipCleanup` option
keeps other tasks' existing builds, including a viewer being kept for use.

| Task | Base | PR destination | Merge method |
| --- | --- | --- | --- |
| Fork tooling, CI or viewer changes | `origin/develop` | fork `develop` | squash |
| Import a stable Aurora release | `origin/develop`, then merge the upstream release tag | fork `develop` | merge |
| Contribute a feature or fix to Aurora | `upstream/main` | Aurora | Aurora's policy |

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

## Synchronize a stable Aurora release into develop

Select the stable tag published in [Aurora's releases](https://github.com/Aurora-Viewer/Aurora-Viewer/releases).
Use that exact tag as the sync target so newer commits on Aurora's `main`
stay outside the release baseline. Create a dedicated task from `origin/develop`:

```powershell
./scripts/new-task.ps1 -Name sync-aurora-v0-9-10 -Kind chore -BaseBranch develop -SkipCleanup
Set-Location ../work/sync-aurora-v0-9-10
git fetch --no-tags upstream 'refs/tags/v0.9.10:refs/tags/v0.9.10'
$upstreamRevision = (git rev-parse 'refs/tags/v0.9.10^{commit}').Trim()
git merge --no-ff $upstreamRevision -m "Merge the Aurora v0.9.10 release into develop"
```

If the release commit is already an ancestor of `develop`, it is already
integrated. Otherwise resolve conflicts, review the combined code against
the exact upstream commit and the fork base, then run `scripts/check.ps1`
and complete human validation when the viewer changes. Keep the tag and
full upstream commit SHA in the PR body.

```powershell
git push -u origin HEAD
gh pr create --repo Minuit-Ferina/Aurora-Viewer --base develop --head chore/sync-aurora-v0-9-10 --title "chore(sync): merge Aurora v0.9.10" --body-file .scratch/pr-body.md
gh pr checks 42 --repo Minuit-Ferina/Aurora-Viewer --watch
```

If `develop` advances while CI runs, merge the latest `origin/develop` into
the sync branch and rerun checks. After review and green required checks,
run `merge-pr.ps1 -Pr 42 -Repository Minuit-Ferina/Aurora-Viewer -Method merge`.
Fetch `origin/develop` after GitHub confirms the merge and verify
`git merge-base --is-ancestor $upstreamRevision origin/develop` exits 0.
Return to the main checkout before running `end-task.ps1` with the fork
repository and the exact PR number.

When preparing a fork release, tag a tested commit of `develop` in the fork.
Build each intended edition from that commit. Keep the previous working
build until the human has validated its replacement.
