<#
.SYNOPSIS
    Requests a ready PR's merge and waits for the outcome, through the merge
    queue when the repository uses one. Run it only when AGENTS.md allows
    it (section 8, step 9). Use -Method merge for upstream synchronization
    so that the imported commits remain ancestors of the fork's main.

.EXAMPLE
    ./scripts/merge-pr.ps1 -Pr 42
.EXAMPLE
    ./scripts/merge-pr.ps1 -Pr 42 -Repository Minuit-Ferina/Aurora-Viewer -Method merge
#>
param(
    [Parameter(Mandatory)][int]$Pr,
    [int]$TimeoutMinutes = 90,
    [int]$GraceSeconds = 60,
    [ValidatePattern('^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$')][string]$Repository,
    [ValidateSet('squash', 'merge')][string]$Method = 'squash'
)
. "$PSScriptRoot\common.ps1"

# An explicit target also supports a PR from this fork to upstream. Never
# rely on gh's shared default repository: every call names the target.
if (-not $Repository) {
    $origin = git -C $PSScriptRoot remote get-url origin
    if ($LASTEXITCODE -ne 0 -or -not $origin) { throw 'Cannot read the origin remote.' }
    if ($origin.Trim() -notmatch '^(?:https://github\.com/|git@github\.com:|ssh://git@github\.com/)(?<owner>[A-Za-z0-9_.-]+)/(?<name>[A-Za-z0-9_.-]+?)(?:\.git)?/?$') {
        throw 'The origin remote must name a GitHub repository.'
    }
    $Repository = "$($Matches.owner)/$($Matches.name)"
}
$owner, $name = $Repository -split '/', 2
# queue entry and auto-merge request: once both are gone while the PR is
# still open, the queue has let it go
$query = 'query($o:String!,$r:String!,$n:Int!){repository(owner:$o,name:$r){pullRequest(number:$n){state mergedAt mergeQueueEntry{state} autoMergeRequest{enabledAt}}}}'
function Get-PrState {
    $j = gh api graphql -f query=$query -F o=$owner -F r=$name -F n=$Pr | ConvertFrom-Json
    $j.data.repository.pullRequest
}

function Exit-Merged($s) {
    $at = if ($s.mergedAt) { " ($(([datetime]$s.mergedAt).ToLocalTime().ToString('HH:mm:ss')))" } else { '' }
    Write-Host "$Repository PR #$Pr merged$at." -ForegroundColor Green
    exit 0
}

# A successful queue run removes the entry a few seconds before GitHub marks
# the PR MERGED (PR #46: entry gone, state still OPEN, MERGED seconds later):
# keep asking for a while before calling it a queue exit. $null while the PR
# is still open and back in the queue (or on auto-merge).
function Wait-LeftQueue {
    $until = (Get-Date).AddSeconds($GraceSeconds)
    $last = $null
    while ((Get-Date) -lt $until) {
        Start-Sleep -Seconds 5
        $s = Get-PrState
        if (-not $s) { continue }
        if ($s.state -ne 'OPEN') { return $s }
        if ($s.mergeQueueEntry -or $s.autoMergeRequest) { return $null }
        $last = $s
    }
    $last
}

# never --admin: the protection of main applies to everyone
$mergeArgs = @('pr', 'merge', $Pr, "--$Method", '--auto', '-R', $Repository)
Invoke-Checked 'gh pr merge' { gh @mergeArgs }
Write-Host "$Repository PR #$Pr waiting for required checks and merge ($Method)…"

$deadline = (Get-Date).AddMinutes($TimeoutMinutes)
$waiting = $false
while ((Get-Date) -lt $deadline) {
    Start-Sleep -Seconds 30
    $s = Get-PrState
    if (-not $s) { continue }
    if ($s.mergeQueueEntry -and -not $waiting) {
        Write-Host "In the queue: tests on the code combined with the latest main…"
        $waiting = $true
    }
    if ($s.state -eq 'OPEN' -and -not $s.mergeQueueEntry -and -not $s.autoMergeRequest) {
        Write-Host "PR #$Pr has no pending merge request: checking the result (up to $GraceSeconds s)…"
        $left = Wait-LeftQueue
        if (-not $left) { continue }
        $s = $left
    }
    if ($s.state -eq 'MERGED') { Exit-Merged $s }
    if ($s.state -eq 'CLOSED') { throw "PR #$Pr closed without being merged." }
    if (-not $s.mergeQueueEntry -and -not $s.autoMergeRequest) {
        Write-Host "PR #$Pr is no longer pending and was not merged. Latest CI runs:" -ForegroundColor Red
        gh run list --workflow CI --limit 3 -R $Repository
        exit 1
    }
}
throw "$Repository PR #$Pr not merged after $TimeoutMinutes min: check its required checks and merge status on GitHub."
