<#
.SYNOPSIS
    Deletes the work folder of a finished task (worktree, local branch,
    target\). Refuses if the matching PR is not merged, its head differs
    from the local branch, or uncommitted changes remain, except with -Force.

.DESCRIPTION
    If a program is still using the folder (terminal, editor, viewer launched
    from the task), git removes the worktree but cannot delete the folder:
    the script says so clearly, and running it again finishes the cleanup
    once the folder is free.

.EXAMPLE
    ./scripts/end-task.ps1 -Name avatar-gaze
.EXAMPLE
    ./scripts/end-task.ps1 -Name avatar-gaze -Repository Aurora-Viewer/Aurora-Viewer -Pr 42
#>
param(
    [Parameter(Mandatory)][string]$Name,
    [switch]$Force,
    [ValidatePattern('^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$')][string]$Repository,
    [ValidateRange(1, 2147483647)][int]$Pr
)
. "$PSScriptRoot\common.ps1"

$p = Get-AuroraPaths
$slug = Get-TaskSlug $Name
$workRoot = [IO.Path]::GetFullPath($p.Work).TrimEnd('\', '/')
$dir = [IO.Path]::GetFullPath((Join-Path $workRoot $slug))
if ((Split-Path $dir -Parent) -ne $workRoot) { throw 'The task folder must be a direct child of work/.' }
if (-not (Test-Path -LiteralPath $dir)) { throw "No work folder $dir." }
# Leave the task folder, PowerShell location and process working folder
# alike: Set-Location alone keeps the process inside it, and Windows then
# refuses to delete the folder (run from the task, as agents often are).
Set-Location $p.Main
[Environment]::CurrentDirectory = $p.Main

$isWorktree = Test-Path -LiteralPath (Join-Path $dir '.git')
if ($isWorktree) {
    # A .git file or folder alone may belong to an independent clone. Never
    # treat failure to remove such a clone as an interrupted worktree cleanup.
    $knownWorktrees = @(git -C $p.Main worktree list --porcelain)
    if ($LASTEXITCODE -ne 0) { throw 'Cannot verify the task worktree registration.' }
    if ($knownWorktrees -notcontains "worktree $($dir -replace '\\', '/')") {
        throw "$dir is not a registered worktree of this repository: nothing is deleted."
    }
    $branch = git -C $dir rev-parse --abbrev-ref HEAD
    if ($LASTEXITCODE -ne 0 -or -not $branch -or $branch.Trim() -eq 'HEAD') { throw 'Cannot identify the task branch.' }
    $branch = $branch.Trim()
} else {
    # leftover of an interrupted clean-up: an empty folder or a copy of this repository
    $empty = -not (Get-ChildItem -LiteralPath $dir -Force | Select-Object -First 1)
    $copy = (Test-Path -LiteralPath (Join-Path $dir 'Cargo.toml')) -and (Test-Path -LiteralPath (Join-Path $dir 'AGENTS.md'))
    if (-not $empty -and -not ($Force -and $copy)) {
        throw "$dir is not a registered worktree. Nonempty leftovers need an explicit -Force after inspection."
    }
    # Without the worktree's HEAD, a matching branch name is not proof that
    # its commits were merged. Leave any local branch for separate inspection.
    $branch = $null
}

if (-not $Force -and $isWorktree) {
    $changes = git -C $dir status --porcelain
    if ($LASTEXITCODE -ne 0) { throw "Cannot inspect changes in $dir." }
    if ($changes) { throw "Uncommitted changes in $dir (use -Force to discard them)." }
    $head = git -C $dir rev-parse HEAD
    if ($LASTEXITCODE -ne 0 -or $head -notmatch '^(?:[0-9a-f]{40}|[0-9a-f]{64})$') { throw 'Cannot read the task commit.' }
    $origin = git -C $p.Main remote get-url origin
    if ($LASTEXITCODE -ne 0 -or -not $origin) { throw 'Cannot read the origin remote.' }
    if ($origin.Trim() -notmatch '^(?:https://github\.com/|git@github\.com:|ssh://git@github\.com/)(?<owner>[A-Za-z0-9_.-]+)/(?<name>[A-Za-z0-9_.-]+?)(?:\.git)?/?$') {
        throw 'The origin remote must name a GitHub repository.'
    }
    $headRepository = "$($Matches.owner)/$($Matches.name)"
    if (-not $Repository) { $Repository = $headRepository }
    $selector = if ($Pr) { $Pr } else { $branch }
    # A PR number belongs to its target repository. Its source must be this
    # fork and this exact branch/commit, even when the target is upstream.
    $json = gh pr view $selector -R $Repository --json state,headRefName,headRefOid,headRepositoryOwner,headRepository 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $json) { throw "Cannot read the PR of $branch in $Repository." }
    try { $pullRequest = $json | ConvertFrom-Json -ErrorAction Stop }
    catch { throw 'GitHub returned invalid pull request metadata.' }
    if ($pullRequest.state -ne 'MERGED') { throw "The PR of $branch in $Repository is not merged." }
    if ($pullRequest.headRefName -cne $branch) { throw 'The merged PR belongs to another branch.' }
    if ($pullRequest.headRefOid -ne $head) { throw 'The local task commit does not match the selected merged PR head.' }
    if (-not $pullRequest.headRepositoryOwner.login -or -not $pullRequest.headRepository.name -or
        "$($pullRequest.headRepositoryOwner.login)/$($pullRequest.headRepository.name)" -ne $headRepository) {
        throw 'The merged PR belongs to another source repository, or its source could not be verified.'
    }
}

if ($isWorktree) {
    $gitArgs = @('worktree', 'remove', $dir)
    if ($Force) { $gitArgs += '--force' }
    git @gitArgs
}
git worktree prune
if ($LASTEXITCODE -ne 0) { throw 'Cannot update worktree registrations.' }
$registered = @(git worktree list --porcelain)
if ($LASTEXITCODE -ne 0) { throw 'Cannot verify worktree registrations.' }
if ($registered -contains "worktree $($dir -replace '\\', '/')") {
    throw "git could not remove the worktree $dir (see the message above)."
}
# git may leave the folder behind when it is in use: delete what remains
if (Test-Path -LiteralPath $dir) {
    Remove-Item -LiteralPath $dir -Recurse -Force -ErrorAction SilentlyContinue
}
if ($branch) { git branch -D $branch 2>$null | Out-Null }

if (Test-Path -LiteralPath $dir) {
    Write-Warning ("The folder $dir could not be fully deleted: a program is using it " +
        "(terminal or editor open in it, viewer launched from the task). Close it, then run " +
        "./scripts/end-task.ps1 -Name $slug again to finish.")
    exit 1
}
Write-Host "Task $slug cleaned up$(if ($branch) { " (local branch $branch deleted)" })." -ForegroundColor Green
