param(
    [Parameter(Mandatory = $true)][string]$Archive,
    [Parameter(Mandatory = $true)][string]$ExpectedSha256,
    [Parameter(Mandatory = $true)][string]$InputPdf,
    [Parameter(Mandatory = $true)][string]$EvidenceDirectory,
    [Parameter(Mandatory = $true)][string]$SkillSource
)

$ErrorActionPreference = "Stop"
if (-not (Test-Path -LiteralPath $Archive -PathType Leaf) -or
    -not (Test-Path -LiteralPath $InputPdf -PathType Leaf)) {
    throw "Archive and input PDF must exist"
}
if (Test-Path -LiteralPath $EvidenceDirectory) {
    throw "Evidence directory already exists: $EvidenceDirectory"
}
foreach ($CommandName in @("npx", "qpdf")) {
    if (-not (Get-Command $CommandName -ErrorAction SilentlyContinue)) {
        throw "$CommandName is required by the acceptance harness"
    }
}

$ActualSha256 = (Get-FileHash -LiteralPath $Archive -Algorithm SHA256).Hash.ToLowerInvariant()
if ($ActualSha256 -ne $ExpectedSha256.ToLowerInvariant()) {
    throw "Archive SHA-256 mismatch: expected $ExpectedSha256, got $ActualSha256"
}

New-Item -ItemType Directory -Path $EvidenceDirectory | Out-Null
Expand-Archive -LiteralPath $Archive -DestinationPath $EvidenceDirectory
$ReleaseRoot = Get-ChildItem -LiteralPath $EvidenceDirectory -Directory -Filter "mimus-*" |
    Select-Object -First 1
if (-not $ReleaseRoot) {
    throw "Archive has no mimus release directory"
}
$Mimus = Join-Path $ReleaseRoot.FullName "mimus.exe"
$RequiredFiles = @(
    "mimus.exe",
    "pdfium.dll",
    "msvcp140.dll",
    "msvcp140_1.dll",
    "vcruntime140.dll",
    "vcruntime140_1.dll"
)
foreach ($RequiredFile in $RequiredFiles) {
    $RequiredPath = Join-Path $ReleaseRoot.FullName $RequiredFile
    if (-not (Test-Path -LiteralPath $RequiredPath -PathType Leaf)) {
        throw "Archive is missing adjacent runtime file: $RequiredFile"
    }
}
$env:MIMUS_CACHE_DIR = Join-Path $EvidenceDirectory "cache"

function Invoke-MimusJson {
    param([string]$Name, [string[]]$Arguments)
    $StdoutPath = Join-Path $EvidenceDirectory "$Name.ndjson"
    $StderrPath = Join-Path $EvidenceDirectory "$Name.stderr"
    $Output = & $Mimus @Arguments 2> $StderrPath
    if ($LASTEXITCODE -ne 0) {
        throw "$Name exited $LASTEXITCODE"
    }
    $Output | Set-Content -LiteralPath $StdoutPath -Encoding utf8
    $Events = @($Output | ForEach-Object { $_ | ConvertFrom-Json })
    if ($Events.Count -eq 0 -or @($Events | Where-Object schema_version -ne 2).Count -ne 0) {
        throw "$Name emitted an invalid schema"
    }
    $Terminals = @($Events | Where-Object { $_.event -eq "result" -or $_.event -eq "error" })
    if ($Terminals.Count -ne 1 -or $Events[-1].event -ne "result") {
        throw "$Name did not end in exactly one result"
    }
    return $Events
}

& $Mimus --help | Set-Content -LiteralPath (Join-Path $EvidenceDirectory "help.txt")
& $Mimus --version | Set-Content -LiteralPath (Join-Path $EvidenceDirectory "version.txt")
$AssetsStarted = Get-Date
$Assets = Invoke-MimusJson "assets" @("--json", "assets", "pull")
$AssetsElapsed = ((Get-Date) - $AssetsStarted).TotalSeconds
if (@($Assets[-1].assets).Count -ne 4) {
    throw "assets pull did not return four assets"
}
$Inspect = Invoke-MimusJson "inspect" @("--json", "inspect", $InputPdf)
$Roundtrip = Join-Path $EvidenceDirectory "roundtrip.pdf"
$Translate = Invoke-MimusJson "translate" @(
    "--json", "translate", $InputPdf, "--backend", "none", "--output", $Roundtrip,
    "--bilingual", "--strip-link-borders"
)
& qpdf --check $Roundtrip *> (Join-Path $EvidenceDirectory "qpdf.txt")
if ($LASTEXITCODE -ne 0) {
    throw "qpdf rejected the roundtrip output"
}

$AgentDirectory = Join-Path $EvidenceDirectory "agent"
New-Item -ItemType Directory -Path $AgentDirectory | Out-Null
Push-Location $AgentDirectory
try {
    $SkillsLog = Join-Path $EvidenceDirectory "skills-add.log"
    & npx --yes skills add $SkillSource --skill mimus --agent codex --copy -y *> $SkillsLog
    if ($LASTEXITCODE -ne 0) {
        throw "Agent Skill installation failed"
    }
} finally {
    Pop-Location
}
if (-not (Test-Path -LiteralPath (Join-Path $AgentDirectory ".agents/skills/mimus/SKILL.md"))) {
    throw "Installed Agent Skill is missing"
}
$AgentInspect = Invoke-MimusJson "agent-inspect" @("--json", "inspect", $InputPdf)
$AgentRoundtrip = Join-Path $EvidenceDirectory "agent-roundtrip.pdf"
$AgentTranslate = Invoke-MimusJson "agent-translate" @(
    "--json", "translate", $InputPdf, "--backend", "none", "--output", $AgentRoundtrip
)
& qpdf --check $AgentRoundtrip *> (Join-Path $EvidenceDirectory "agent-qpdf.txt")
if ($LASTEXITCODE -ne 0) {
    throw "qpdf rejected the agent roundtrip output"
}

$CacheBytes = (Get-ChildItem -LiteralPath $env:MIMUS_CACHE_DIR -File -Recurse |
    Measure-Object -Property Length -Sum).Sum
$Summary = [ordered]@{
    version = (Get-Content -LiteralPath (Join-Path $EvidenceDirectory "version.txt") -Raw).Trim()
    archive_sha256 = $ActualSha256
    input_sha256 = (Get-FileHash -LiteralPath $InputPdf -Algorithm SHA256).Hash.ToLowerInvariant()
    asset_count = 4
    asset_cache_bytes = $CacheBytes
    assets_elapsed_seconds = $AssetsElapsed
    terminal_events = [ordered]@{
        assets_pull = "result"
        inspect = "result"
        translate = "result"
        agent_inspect = "result"
        agent_translate = "result"
    }
    qpdf = "passed"
    skill_install = "passed"
}
$SummaryPath = Join-Path $EvidenceDirectory "summary.json"
$Summary | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $SummaryPath -Encoding utf8
Write-Output "release install verification passed: $EvidenceDirectory"
