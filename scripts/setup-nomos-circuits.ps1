param(
    [string]$Version    = "v0.3.1",
    [string]$InstallDir = "$env:USERPROFILE\.nomos-circuits"
)

$Repo               = "logos-co/nomos-circuits"
$DefaultInstallDir  = "$env:USERPROFILE\.nomos-circuits"

# --- Pretty printing helpers -------------------------------------------------
function Write-Info($Message) {
    Write-Host "[INFO] " -ForegroundColor Cyan -NoNewline
    Write-Host " $Message"
}

function Write-Ok($Message) {
    Write-Host "[OK]   " -ForegroundColor Green -NoNewline
    Write-Host " $Message"
}

function Write-Warn($Message) {
    Write-Host "[WARN] " -ForegroundColor Yellow -NoNewline
    Write-Host " $Message"
}

function Write-ErrMsg($Message) {
    Write-Host "[ERROR]" -ForegroundColor Red -NoNewline
    Write-Host " $Message"
}

# --- Detect platform (Windows-only script) -----------------------------------
function Get-Platform {
    # We only support Windows in this script
    $os = "windows"

    # Architecture
    switch ($env:PROCESSOR_ARCHITECTURE) {
        "AMD64" { $arch = "x86_64" }
        "x86"   { $arch = "x86_64" } # 32-bit shell on 64-bit OS – binaries are still x86_64
        default {
            Write-ErrMsg "Unsupported architecture: $($env:PROCESSOR_ARCHITECTURE)"
            exit 1
        }
    }

    return "$os-$arch"
}

# --- Check existing installation ---------------------------------------------
function Check-ExistingInstallation {
    param(
        [string]$InstallDir
    )

    if (Test-Path $InstallDir) {
        Write-Warn "Installation directory already exists: $InstallDir"

        $versionFile = Join-Path $InstallDir "VERSION"
        if (Test-Path $versionFile) {
            $currentVersion = Get-Content $versionFile -ErrorAction SilentlyContinue
            if ($null -ne $currentVersion -and $currentVersion -ne "") {
                Write-Info "Currently installed version: $currentVersion"
            }
        }

        # Non-interactive: auto-overwrite
        if (-not ([Environment]::UserInteractive)) {
            Write-Info "Non-interactive environment detected, automatically overwriting..."
        } else {
            Write-Host
            $response = Read-Host "Do you want to overwrite it? (y/N)"
            if ($response -notin @("y", "Y")) {
                Write-Info "Installation cancelled."
                exit 0
            }
        }

        Write-Info "Removing existing installation..."
        Remove-Item -Recurse -Force $InstallDir
    }
}

# --- Download and extract (tries .tar.gz then .zip) --------------------------
function Download-And-Extract {
    param(
        [string]$Platform,
        [string]$Version,
        [string]$Repo,
        [string]$InstallDir
    )

    $artifacts = @(
        "nomos-circuits-$Version-$Platform.tar.gz",
        "nomos-circuits-$Version-$Platform.zip"
    )

    $tempDir = Join-Path ([System.IO.Path]::GetTempPath()) ([System.Guid]::NewGuid().ToString())
    New-Item -ItemType Directory -Path $tempDir -Force | Out-Null

    foreach ($artifact in $artifacts) {
        $url = "https://github.com/$Repo/releases/download/$Version/$artifact"
        Write-Info "Downloading nomos-circuits $Version for $Platform"
        Write-Info "URL: $url"

        $archivePath = Join-Path $tempDir $artifact

        try {
            if ($env:GITHUB_TOKEN) {
                Invoke-WebRequest -Uri $url `
                                  -Headers @{ Authorization = "Bearer $env:GITHUB_TOKEN" } `
                                  -OutFile $archivePath `
                                  -UseBasicParsing
            } else {
                Invoke-WebRequest -Uri $url `
                                  -OutFile $archivePath `
                                  -UseBasicParsing
            }

            Write-Ok "Download complete: $artifact"

            Write-Info "Extracting to $InstallDir ..."
            if (-not (Test-Path $InstallDir)) {
                New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
            }

            if ($artifact.ToLower().EndsWith(".tar.gz")) {
                # Windows 10+ has `tar` available
                tar -xzf $archivePath -C $InstallDir --strip-components 1
            } else {
                # ZIP fallback
                Expand-Archive -Path $archivePath -DestinationPath $InstallDir -Force
                # If zip contains a top-level directory, user can reorganize if needed
            }

            Write-Ok "Extraction complete"
            Remove-Item -Recurse -Force $tempDir
            return
        }
        catch {
            Write-Warn ("Failed to download or extract {0}: {1}" -f $artifact, $_.Exception.Message)
            if (Test-Path $archivePath) {
                Remove-Item -Force $archivePath
            }
            # Try next artifact
        }
    }

    # If we reach here, all artifact attempts failed
    Remove-Item -Recurse -Force $tempDir -ErrorAction SilentlyContinue
    Write-ErrMsg "Unable to download nomos-circuits for $Platform and version $Version."
    Write-ErrMsg "Check releases at: https://github.com/$Repo/releases"
    exit 1
}

# --- Main --------------------------------------------------------------------
Write-Info "Setting up nomos-circuits $Version"
Write-Info "Installation directory: $InstallDir"
Write-Host

$platform = Get-Platform
Write-Info "Detected platform: $platform"

Check-ExistingInstallation -InstallDir $InstallDir
Download-And-Extract -Platform $platform -Version $Version -Repo $Repo -InstallDir $InstallDir

Write-Host
Write-Ok "Installation complete!"
Write-Host
Write-Info "nomos-circuits $Version is now installed at: $InstallDir"
Write-Info "The following circuits are available:"

# discover circuits: directories containing witness_generator or witness_generator.exe
if (Test-Path $InstallDir) {
    Get-ChildItem -Path $InstallDir -Directory | ForEach-Object {
        $wg1 = Join-Path $_.FullName "witness_generator"
        $wg2 = Join-Path $_.FullName "witness_generator.exe"
        if ( (Test-Path $wg1) -or (Test-Path $wg2) ) {
            Write-Host "  - $($_.Name)"
        }
    }
}

if ($InstallDir -ne $DefaultInstallDir) {
    Write-Host
    Write-Info "Since you're using a custom installation directory, set the environment variable:"
    Write-Host "  `$env:NOMOS_CIRCUITS = `"$InstallDir`""
    Write-Host
}
