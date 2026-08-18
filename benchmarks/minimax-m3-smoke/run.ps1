[CmdletBinding()]
param(
    [ValidateSet("iagent", "claude", "codex", "opencode")]
    [string[]]$Agents = @("iagent", "claude", "codex", "opencode"),

    [ValidateSet("bugfix", "data-report")]
    [string[]]$Tasks = @("bugfix", "data-report"),

    [ValidateRange(60, 1800)]
    [int]$TimeoutSeconds = 600,

    [switch]$SkipBuild,

    [switch]$KeepExisting
)

$ErrorActionPreference = "Stop"
$Utf8NoBom = [System.Text.UTF8Encoding]::new($false)
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
$RunsRoot = Join-Path $PSScriptRoot ".runs"
$HomesRoot = Join-Path $PSScriptRoot ".homes"
$TasksFile = Join-Path $PSScriptRoot "tasks.json"

$ApiKey = if ($env:ANTHROPIC_AUTH_TOKEN) {
    $env:ANTHROPIC_AUTH_TOKEN
} elseif ($env:ANTHROPIC_API_KEY) {
    $env:ANTHROPIC_API_KEY
} else {
    $null
}
if (-not $ApiKey) {
    throw "请先设置 ANTHROPIC_AUTH_TOKEN 或 ANTHROPIC_API_KEY；runner 不从文件读取、也不保存密钥。"
}

$Model = if ($env:ANTHROPIC_MODEL) {
    $env:ANTHROPIC_MODEL
} elseif ($env:ANTHROPIC_DEFAULT_HAIKU_MODEL) {
    $env:ANTHROPIC_DEFAULT_HAIKU_MODEL
} else {
    "MiniMax-M3"
}
$AnthropicBaseUrl = if ($env:ANTHROPIC_BASE_URL) {
    $env:ANTHROPIC_BASE_URL.TrimEnd("/")
} else {
    "https://api.minimaxi.com/anthropic"
}
$AnthropicV1 = if ($AnthropicBaseUrl.EndsWith("/v1")) {
    $AnthropicBaseUrl
} else {
    "$AnthropicBaseUrl/v1"
}
$ResponsesBaseUrl = if ($env:MINIMAX_RESPONSES_BASE_URL) {
    $env:MINIMAX_RESPONSES_BASE_URL.TrimEnd("/")
} else {
    "https://api.minimaxi.com/v1"
}

if (-not $SkipBuild) {
    & cargo build --release --locked --manifest-path (Join-Path $RepoRoot "Cargo.toml")
    if ($LASTEXITCODE -ne 0) { throw "i-agent release 构建失败" }
}
$IagentExe = Join-Path $RepoRoot "target\release\i-agent.exe"
if (-not (Test-Path $IagentExe)) {
    throw "找不到 $IagentExe；请去掉 -SkipBuild 或先执行 cargo build --release。"
}

if (-not $KeepExisting) {
    foreach ($dir in @($RunsRoot, $HomesRoot)) {
        if (Test-Path $dir) { Remove-Item $dir -Recurse -Force -Confirm:$false }
    }
}
foreach ($dir in @($RunsRoot, $HomesRoot)) {
    New-Item -ItemType Directory -Force $dir | Out-Null
}

$ClaudeHome = Join-Path $HomesRoot "claude"
$CodexHome = Join-Path $HomesRoot "codex"
$OpenCodeHome = Join-Path $HomesRoot "opencode"
foreach ($dir in @($ClaudeHome, $CodexHome, $OpenCodeHome)) {
    New-Item -ItemType Directory -Force $dir | Out-Null
}

$OpenCodeModels = [ordered]@{}
$OpenCodeModels[$Model] = [ordered]@{
    name = $Model
    limit = [ordered]@{ context = 204800; output = 32768 }
}
$OpenCodeConfig = [ordered]@{
    '$schema' = "https://opencode.ai/config.json"
    provider = [ordered]@{
        minimax = [ordered]@{
            npm = "@ai-sdk/anthropic"
            name = "MiniMax Anthropic"
            options = [ordered]@{
                baseURL = $AnthropicV1
                apiKey = "{env:ANTHROPIC_AUTH_TOKEN}"
            }
            models = $OpenCodeModels
        }
    }
    model = "minimax/$Model"
    small_model = "minimax/$Model"
}
$OpenCodeConfigPath = Join-Path $OpenCodeHome "opencode.json"
[System.IO.File]::WriteAllText(
    $OpenCodeConfigPath,
    ($OpenCodeConfig | ConvertTo-Json -Depth 12),
    $Utf8NoBom
)

$SafeResponsesBase = $ResponsesBaseUrl.Replace('"', '')
$CodexConfig = @"
model = "$Model"
model_provider = "minimax"
approval_policy = "never"
sandbox_mode = "danger-full-access"
model_reasoning_effort = "low"
model_supports_reasoning_summaries = false
model_context_window = 204800
model_max_output_tokens = 32768

[model_providers.minimax]
name = "MiniMax Responses"
base_url = "$SafeResponsesBase"
env_key = "MINIMAX_BENCH_API_KEY"
wire_api = "responses"
request_max_retries = 2
stream_max_retries = 2
"@
[System.IO.File]::WriteAllText((Join-Path $CodexHome "config.toml"), $CodexConfig, $Utf8NoBom)

function Get-LaunchSpec {
    param([Parameter(Mandatory)][string]$CommandName)
    $command = Get-Command $CommandName -ErrorAction Stop | Select-Object -First 1
    if ($command.Source.EndsWith(".ps1", [System.StringComparison]::OrdinalIgnoreCase)) {
        $pwsh = (Get-Command pwsh -ErrorAction Stop | Select-Object -First 1).Source
        return [pscustomobject]@{
            FilePath = $pwsh
            Prefix = @("-NoProfile", "-NonInteractive", "-File", $command.Source)
        }
    }
    return [pscustomobject]@{ FilePath = $command.Source; Prefix = @() }
}

function Get-CommandVersion {
    param([Parameter(Mandatory)][string]$CommandName)
    try {
        $text = (& $CommandName --version 2>&1 | Out-String).Trim()
        if ($text) { return ($text -split "`r?`n")[0] }
    } catch { }
    return "unknown"
}

function Invoke-CapturedProcess {
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [Parameter(Mandatory)][string[]]$Arguments,
        [Parameter(Mandatory)][string]$WorkingDirectory,
        [Parameter(Mandatory)][hashtable]$Environment,
        [Parameter(Mandatory)][string]$StdoutPath,
        [Parameter(Mandatory)][string]$StderrPath,
        [Parameter(Mandatory)][int]$Timeout
    )

    $psi = [System.Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $FilePath
    $psi.WorkingDirectory = $WorkingDirectory
    $psi.UseShellExecute = $false
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $psi.CreateNoWindow = $true
    foreach ($arg in $Arguments) { [void]$psi.ArgumentList.Add($arg) }

    foreach ($name in @(
        "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_BASE_URL", "ANTHROPIC_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL", "ANTHROPIC_DEFAULT_SONNET_MODEL", "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_SMALL_FAST_MODEL", "I_AGENT_API_KEY", "I_AGENT_BASE_URL", "I_AGENT_MODEL",
        "I_AGENT_PROTOCOL", "MINIMAX_API_KEY", "CLAUDECODE", "CLAUDE_CODE_EXECPATH", "AI_AGENT"
    )) {
        [void]$psi.Environment.Remove($name)
    }
    foreach ($entry in $Environment.GetEnumerator()) {
        $psi.Environment[$entry.Key] = [string]$entry.Value
    }

    $process = [System.Diagnostics.Process]::new()
    $process.StartInfo = $psi
    $started = [System.Diagnostics.Stopwatch]::StartNew()
    if (-not $process.Start()) { throw "无法启动 $FilePath" }
    $process.StandardInput.Close()
    $stdoutTask = $process.StandardOutput.ReadToEndAsync()
    $stderrTask = $process.StandardError.ReadToEndAsync()
    $finished = $process.WaitForExit($Timeout * 1000)
    $timedOut = -not $finished
    if ($timedOut) {
        try { $process.Kill($true) } catch { }
    }
    $process.WaitForExit()
    $started.Stop()
    $stdout = $stdoutTask.GetAwaiter().GetResult()
    $stderr = $stderrTask.GetAwaiter().GetResult()
    [System.IO.File]::WriteAllText($StdoutPath, $stdout, $Utf8NoBom)
    [System.IO.File]::WriteAllText($StderrPath, $stderr, $Utf8NoBom)
    $exitCode = if ($timedOut) { 124 } else { $process.ExitCode }
    $process.Dispose()
    return [pscustomobject]@{
        ExitCode = $exitCode
        TimedOut = $timedOut
        WallSeconds = [Math]::Round($started.Elapsed.TotalSeconds, 1)
    }
}

$versions = @{
    iagent = (& $IagentExe -V 2>&1 | Out-String).Trim()
    claude = Get-CommandVersion "claude"
    codex = Get-CommandVersion "codex"
    opencode = Get-CommandVersion "opencode"
}
$taskConfig = Get-Content $TasksFile -Raw -Encoding UTF8 | ConvertFrom-Json -AsHashtable
$commonEnv = @{
    ANTHROPIC_AUTH_TOKEN = $ApiKey
    ANTHROPIC_API_KEY = $ApiKey
    ANTHROPIC_BASE_URL = $AnthropicBaseUrl
    ANTHROPIC_MODEL = $Model
    ANTHROPIC_DEFAULT_HAIKU_MODEL = $Model
    ANTHROPIC_DEFAULT_SONNET_MODEL = $Model
    ANTHROPIC_DEFAULT_OPUS_MODEL = $Model
    ANTHROPIC_SMALL_FAST_MODEL = $Model
    MINIMAX_BENCH_API_KEY = $ApiKey
    CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT = "1"
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC = "1"
    DISABLE_AUTOUPDATER = "1"
    DISABLE_TELEMETRY = "1"
    DISABLE_ERROR_REPORTING = "1"
    NO_PROXY = "127.0.0.1,localhost"
}

foreach ($task in $Tasks) {
    $spec = $taskConfig[$task]
    foreach ($agent in $Agents) {
        $workspace = Join-Path $RunsRoot "$task\$agent"
        if (Test-Path $workspace) { Remove-Item $workspace -Recurse -Force -Confirm:$false }
        New-Item -ItemType Directory -Force $workspace | Out-Null
        $fixture = Join-Path $PSScriptRoot $spec.fixture
        Copy-Item (Join-Path $fixture "*") $workspace -Recurse -Force

        $envForRun = $commonEnv.Clone()
        $protocol = "anthropic-messages"
        switch ($agent) {
            "iagent" {
                $launch = [pscustomobject]@{ FilePath = $IagentExe; Prefix = @() }
                $arguments = @("-C", $workspace, "-p", $spec.prompt)
                $envForRun["I_AGENT_ASSETS"] = (Join-Path $RepoRoot "assets")
            }
            "claude" {
                $launch = Get-LaunchSpec "claude"
                $arguments = @("-p", $spec.prompt, "--dangerously-skip-permissions", "--output-format", "json", "--no-session-persistence", "--bare", "--disable-slash-commands")
                $envForRun["CLAUDE_CONFIG_DIR"] = $ClaudeHome
            }
            "codex" {
                $launch = Get-LaunchSpec "codex"
                $arguments = @("exec", "--cd", $workspace, "--skip-git-repo-check", "--dangerously-bypass-approvals-and-sandbox", "--ephemeral", "--json", $spec.prompt)
                $envForRun["CODEX_HOME"] = $CodexHome
                $protocol = "openai-responses"
            }
            "opencode" {
                $launch = Get-LaunchSpec "opencode"
                $arguments = @("run", "--pure", "--auto", "--format", "json", "--model", "minimax/$Model", "--dir", $workspace, $spec.prompt)
                $envForRun["OPENCODE_CONFIG"] = $OpenCodeConfigPath
                $envForRun["XDG_CONFIG_HOME"] = (Join-Path $OpenCodeHome "config")
                $envForRun["XDG_DATA_HOME"] = (Join-Path $OpenCodeHome "data")
                $envForRun["OPENCODE_DISABLE_AUTOUPDATE"] = "1"
            }
        }

        $allArguments = @($launch.Prefix) + $arguments
        Write-Host "[$task/$agent] MiniMax-M3 via $protocol ..."
        $result = Invoke-CapturedProcess `
            -FilePath $launch.FilePath `
            -Arguments $allArguments `
            -WorkingDirectory $workspace `
            -Environment $envForRun `
            -StdoutPath (Join-Path $workspace "_stdout.txt") `
            -StderrPath (Join-Path $workspace "_stderr.txt") `
            -Timeout $TimeoutSeconds

        $meta = [ordered]@{
            task = $task
            agent = $agent
            version = $versions[$agent]
            protocol = $protocol
            exit_code = $result.ExitCode
            timed_out = $result.TimedOut
            wall_seconds = $result.WallSeconds
            finished_at = [DateTime]::UtcNow.ToString("o")
        }
        [System.IO.File]::WriteAllText(
            (Join-Path $workspace "_meta.json"),
            ($meta | ConvertTo-Json -Depth 5),
            $Utf8NoBom
        )
        Write-Host "  exit=$($result.ExitCode) wall=$($result.WallSeconds)s"
    }
}

$LocalResults = Join-Path $PSScriptRoot "results.local.json"
& python (Join-Path $PSScriptRoot "summarize.py") --runs $RunsRoot --output $LocalResults
if ($LASTEXITCODE -ne 0) { throw "汇总脚本失败" }
Write-Host "完成。脱敏本地汇总: $LocalResults"
