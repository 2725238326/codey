# Read-only diagnostics. Does not launch Codex, change registration, or read auth files.
[CmdletBinding()]
param(
    [DateTimeOffset]$At = [DateTimeOffset]::Now,
    [ValidateRange(1, 60)]
    [int]$WindowMinutes = 5
)

$ErrorActionPreference = 'Stop'
if ($env:OS -ne 'Windows_NT') {
    throw 'Run this script on the Windows computer where Codex failed to start.'
}

$start = $At.AddMinutes(-$WindowMinutes).LocalDateTime
$end = $At.AddMinutes($WindowMinutes).LocalDateTime
$os = Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion'
$packages = @()
$packageError = $null
try {
    $packages = @(Get-AppxPackage -Name 'OpenAI.Codex' | ForEach-Object {
        $package = $_
        $applications = @()
        $manifestError = $null
        try {
            $manifest = Get-AppxPackageManifest -Package $package.PackageFullName
            $applications = @($manifest.SelectNodes("//*[local-name()='Applications']/*[local-name()='Application']") | ForEach-Object {
                $application = $_
                $attributes = [ordered]@{}
                foreach ($attribute in $application.Attributes) {
                    if ($attribute.LocalName -in @('Id', 'Executable', 'EntryPoint', 'RuntimeBehavior', 'TrustLevel')) {
                        $attributes[$attribute.LocalName] = $attribute.Value
                    }
                }
                [ordered]@{
                    attributes = $attributes
                    executionAliases = @($application.SelectNodes(".//*[local-name()='ExecutionAlias']") | ForEach-Object {
                        $_.GetAttribute('Alias')
                    })
                }
            })
        } catch {
            $manifestError = $_.Exception.Message
        }
        [ordered]@{
            packageFullName = $package.PackageFullName
            packageFamilyName = $package.PackageFamilyName
            installLocation = $package.InstallLocation
            architecture = [string]$package.Architecture
            status = [string]$package.Status
            signatureKind = [string]$package.SignatureKind
            applications = $applications
            manifestError = $manifestError
            dependencies = @($package.Dependencies | ForEach-Object {
                [ordered]@{ packageFullName = $_.PackageFullName; status = [string]$_.Status }
            })
        }
    })
} catch {
    $packageError = $_.Exception.Message
}

$channels = @(
    'Microsoft-Windows-AppModel-Runtime/Admin',
    'Microsoft-Windows-AppModel-Runtime/Operational',
    'Microsoft-Windows-TWinUI/Operational',
    'Application'
)
$eventLogs = @($channels | ForEach-Object {
    $channel = $_
    $events = @()
    $readError = $null
    $scanned = 0
    try {
        $candidates = @(Get-WinEvent -FilterHashtable @{
            LogName = $channel
            StartTime = $start
            EndTime = $end
        } -MaxEvents 500)
        $scanned = $candidates.Count
        $events = @($candidates | ForEach-Object {
            $event = $_
            $xml = $event.ToXml()
            $message = $event.Message
            # Retain only this app's events, not other applications' records.
            if ($xml -match '(?i)OpenAI\.Codex|Codex\.exe' -or $message -match '(?i)OpenAI\.Codex|Codex\.exe') {
                [ordered]@{
                    time = $event.TimeCreated.ToString('o')
                    id = $event.Id
                    recordId = $event.RecordId
                    provider = $event.ProviderName
                    level = $event.LevelDisplayName
                    message = $message
                    xml = $xml
                }
            }
        })
    } catch {
        if ($_.FullyQualifiedErrorId -notlike 'NoMatchingEventsFound*') {
            $readError = $_.Exception.Message
        }
    }
    [ordered]@{
        channel = $channel
        scanned = $scanned
        scanLimitReached = ($scanned -eq 500)
        readError = $readError
        events = $events
    }
})

[ordered]@{
    collectedAt = [DateTimeOffset]::Now.ToString('o')
    incidentAt = $At.ToString('o')
    windowMinutes = $WindowMinutes
    windows = [ordered]@{
        productName = $os.ProductName
        displayVersion = $os.DisplayVersion
        build = $os.CurrentBuildNumber
        revision = $os.UBR
        architecture = $env:PROCESSOR_ARCHITECTURE
    }
    packageError = $packageError
    packages = $packages
    eventLogs = $eventLogs
} | ConvertTo-Json -Depth 12
