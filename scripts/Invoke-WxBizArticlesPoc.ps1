[CmdletBinding()]
param(
    [string]$ReleaseBinary,
    [ValidateRange(1, 5000)]
    [int]$ArticleLimit = 200
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Upstream installation and command contract:
# https://github.com/jackwener/wx-cli-again#安装
# https://github.com/jackwener/wx-cli-again#公众号文章
# Pinned source baseline used by this project:
# https://github.com/jackwener/wx-cli-again/commit/077a54cbfe679bda963cd038d8440422907fc797

$ProjectRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$ExpectedVersion = '0.6.3'
$LocalBinDir = Join-Path $ProjectRoot '.local\bin'
$WxPath = Join-Path $LocalBinDir 'wx.exe'

function Invoke-WxText {
    param(
        [Parameter(Mandatory)]
        [string]$Executable,
        [Parameter(Mandatory)]
        [string[]]$Arguments
    )

    $stderrPath = Join-Path ([System.IO.Path]::GetTempPath()) (
        'wx-cli-poc-stderr-{0}.txt' -f [guid]::NewGuid().ToString('N')
    )
    try {
        $stdout = & $Executable @Arguments 2> $stderrPath
        $exitCode = $LASTEXITCODE
        $stderr = if (Test-Path -LiteralPath $stderrPath) {
            Get-Content -Raw -LiteralPath $stderrPath
        } else {
            ''
        }
        if ($exitCode -ne 0) {
            $message = if ([string]::IsNullOrWhiteSpace($stderr)) {
                "wx 失败，退出码 $exitCode"
            } else {
                $stderr.Trim()
            }
            throw $message
        }
        return ($stdout -join [Environment]::NewLine)
    } finally {
        if (Test-Path -LiteralPath $stderrPath) {
            Remove-Item -LiteralPath $stderrPath -Force
        }
    }
}

function Find-ReleaseBinary {
    param([string]$ExplicitPath)

    $candidates = [System.Collections.Generic.List[string]]::new()
    if (-not [string]::IsNullOrWhiteSpace($ExplicitPath)) {
        $candidates.Add($ExplicitPath)
    }
    $candidates.Add((Join-Path $ProjectRoot 'wx-windows-x86_64.exe'))
    $candidates.Add((Join-Path $ProjectRoot '.local\downloads\wx-windows-x86_64.exe'))
    $candidates.Add((Join-Path $env:USERPROFILE 'Downloads\wx-windows-x86_64.exe'))

    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }
    return $null
}

function Find-ActionsArtifactZip {
    $candidates = @(
        (Join-Path $ProjectRoot 'wx-windows-x86_64.zip'),
        (Join-Path $env:USERPROFILE 'Downloads\wx-windows-x86_64.zip')
    )

    foreach ($candidate in $candidates) {
        if (Test-Path -LiteralPath $candidate -PathType Leaf) {
            return (Resolve-Path -LiteralPath $candidate).Path
        }
    }
    return $null
}

function Expand-ActionsArtifact {
    param([Parameter(Mandatory)][string]$ZipPath)

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $downloadDir = Join-Path $ProjectRoot '.local\downloads'
    $destination = Join-Path $downloadDir 'wx-windows-x86_64.exe'
    New-Item -ItemType Directory -Path $downloadDir -Force | Out-Null

    $archive = [System.IO.Compression.ZipFile]::OpenRead($ZipPath)
    try {
        $matches = @(
            $archive.Entries | Where-Object {
                [System.IO.Path]::GetFileName($_.FullName) -eq 'wx-windows-x86_64.exe'
            }
        )
        if ($matches.Count -ne 1) {
            throw "Actions 构件中应恰好包含一个 wx-windows-x86_64.exe，实际为 $($matches.Count) 个。"
        }

        $inputStream = $matches[0].Open()
        try {
            $outputStream = [System.IO.File]::Open(
                $destination,
                [System.IO.FileMode]::Create,
                [System.IO.FileAccess]::Write,
                [System.IO.FileShare]::None
            )
            try {
                $inputStream.CopyTo($outputStream)
            } finally {
                $outputStream.Dispose()
            }
        } finally {
            $inputStream.Dispose()
        }
    } finally {
        $archive.Dispose()
    }

    return $destination
}

function Install-ProjectLocalWx {
    param([string]$SourcePath)

    New-Item -ItemType Directory -Path $LocalBinDir -Force | Out-Null
    $sourceHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $SourcePath).Hash
    $installedHash = if (Test-Path -LiteralPath $WxPath -PathType Leaf) {
        (Get-FileHash -Algorithm SHA256 -LiteralPath $WxPath).Hash
    } else {
        $null
    }
    if ($sourceHash -ne $installedHash) {
        Copy-Item -LiteralPath $SourcePath -Destination $WxPath -Force
    }

    $versionText = (Invoke-WxText -Executable $WxPath -Arguments @('--version')).Trim()
    if ($versionText -notmatch "(^|\s)$([regex]::Escape($ExpectedVersion))(\s|$)") {
        Remove-Item -LiteralPath $WxPath -Force
        throw "Release 版本不匹配：期望 $ExpectedVersion，实际为 '$versionText'。"
    }

    return [pscustomobject]@{
        Path = $WxPath
        Version = $versionText
        Sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $WxPath).Hash.ToLowerInvariant()
    }
}

$release = Find-ReleaseBinary -ExplicitPath $ReleaseBinary
if (-not $release -and [string]::IsNullOrWhiteSpace($ReleaseBinary)) {
    $artifactZip = Find-ActionsArtifactZip
    if ($artifactZip) {
        $release = Expand-ActionsArtifact -ZipPath $artifactZip
    }
}
if (-not $release -and -not (Test-Path -LiteralPath $WxPath -PathType Leaf)) {
    throw @"
未找到 wx-windows-x86_64.exe。
请先在自己的 GitHub 仓库运行 Actions 工作流“Build Windows binary”，下载构件，并将以下任一文件放到项目根目录：
  $ProjectRoot\wx-windows-x86_64.zip
  $ProjectRoot\wx-windows-x86_64.exe
然后重新运行本脚本。
"@
}

if ($release) {
    $installation = Install-ProjectLocalWx -SourcePath $release
} else {
    $versionText = (Invoke-WxText -Executable $WxPath -Arguments @('--version')).Trim()
    if ($versionText -notmatch "(^|\s)$([regex]::Escape($ExpectedVersion))(\s|$)") {
        throw "项目本地 wx.exe 版本不匹配：期望 $ExpectedVersion，实际为 '$versionText'。"
    }
    $installation = [pscustomobject]@{
        Path = $WxPath
        Version = $versionText
        Sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $WxPath).Hash.ToLowerInvariant()
    }
}

$doctorText = Invoke-WxText -Executable $WxPath -Arguments @('doctor', '--json')
$doctor = $doctorText | ConvertFrom-Json
$checks = @($doctor.checks)

$blockingNames = @('config.json', '数据库密钥', '关键分片密钥', 'SQLCipher 在线打开')
$blockingFailures = @(
    $checks | Where-Object {
        $_.name -in $blockingNames -and -not $_.ok
    }
)

if ($blockingFailures.Count -gt 0) {
    $failedNames = ($blockingFailures | ForEach-Object { $_.name }) -join '、'
    Write-Output ([pscustomobject]@{
        status = 'needs_init_or_keys'
        wx_version = $installation.Version
        wx_sha256 = $installation.Sha256
        failed_checks = @($blockingFailures | ForEach-Object { $_.name })
        next_command = "以管理员 PowerShell 运行：& '$WxPath' init"
    } | ConvertTo-Json -Depth 5)
    throw "POC 前置检查未通过：$failedNames"
}

$articlesText = Invoke-WxText -Executable $WxPath -Arguments @(
    'biz-articles', '-n', $ArticleLimit.ToString(), '--json'
)
$articles = @($articlesText | ConvertFrom-Json)
$requiredFields = @(
    'account',
    'account_username',
    'title',
    'url',
    'timestamp',
    'recv_time',
    'recv_time_str'
)

$fieldCoverage = [ordered]@{}
foreach ($field in $requiredFields) {
    $present = @(
        $articles | Where-Object {
            $property = $_.PSObject.Properties[$field]
            $null -ne $property -and -not [string]::IsNullOrWhiteSpace([string]$property.Value)
        }
    ).Count
    $fieldCoverage[$field] = [pscustomobject]@{
        present = $present
        total = $articles.Count
    }
}

$urlClasses = @(
    $articles | ForEach-Object {
        $uri = $null
        $isHttpUrl = [uri]::TryCreate(
            [string]$_.url,
            [System.UriKind]::Absolute,
            [ref]$uri
        ) -and $uri.Scheme -in @('http', 'https')
        if (-not $isHttpUrl) {
            'invalid'
        } elseif ($uri.Host -eq 'mp.weixin.qq.com') {
            'weixin_article'
        } else {
            'external_http'
        }
    }
)
$invalidUrlCount = @($urlClasses | Where-Object { $_ -eq 'invalid' }).Count
$weixinArticleUrlCount = @($urlClasses | Where-Object { $_ -eq 'weixin_article' }).Count
$externalHttpUrlCount = @($urlClasses | Where-Object { $_ -eq 'external_http' }).Count
$missingRequiredFieldCount = @(
    $fieldCoverage.GetEnumerator() | Where-Object {
        $_.Value.present -ne $_.Value.total
    }
).Count

$summary = [ordered]@{
    status = if (
        $articles.Count -gt 0 -and
        $invalidUrlCount -eq 0 -and
        $missingRequiredFieldCount -eq 0
    ) { 'poc_query_ok' } else { 'poc_needs_review' }
    wx_version = $installation.Version
    wx_sha256 = $installation.Sha256
    article_count = $articles.Count
    unique_account_count = @($articles.account_username | Where-Object { $_ } | Sort-Object -Unique).Count
    weixin_article_url_count = $weixinArticleUrlCount
    external_http_url_count = $externalHttpUrlCount
    invalid_http_url_count = $invalidUrlCount
    field_coverage = $fieldCoverage
    privacy = '摘要不包含公众号名称、文章标题、URL、数据库路径或密钥。'
}

Write-Output ($summary | ConvertTo-Json -Depth 6)
