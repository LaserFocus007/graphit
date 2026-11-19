# Installs Piper and natural voices for reading aloud in Graphit on Windows.
# Usage: install-voice.ps1 -Dir <target folder> [-Langs "de en"]
# Graphit passes its own data folder. About 140 MB per language, 250 MB for both.
param([Parameter(Mandatory = $true)][string]$Dir, [string]$Langs = "de en")
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
New-Item -ItemType Directory -Force -Path $Dir | Out-Null
Set-Location $Dir
# Download to a temporary name first, so an interrupted download never counts as installed
function Get-File([string]$Url, [string]$Out) {
    Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile "$Out.part"
    Move-Item -Force "$Out.part" $Out
}
if (-not (Test-Path "piper\piper.exe")) {
    Get-File "https://github.com/rhasspy/piper/releases/download/2023.11.14-2/piper_windows_amd64.zip" "piper.zip"
    Expand-Archive -Force "piper.zip" "."
    Remove-Item "piper.zip"
}
$Base = "https://huggingface.co/rhasspy/piper-voices/resolve/main"
foreach ($L in $Langs.Split(" ", [StringSplitOptions]::RemoveEmptyEntries)) {
    switch ($L) {
        "de" { $Voice = "de_DE-thorsten-high"; $Sub = "de/de_DE/thorsten/high" }
        "en" { $Voice = "en_US-lessac-high"; $Sub = "en/en_US/lessac/high" }
        default { throw "Unknown language $L" }
    }
    if (-not (Test-Path "$Voice.onnx.json")) { Get-File "$Base/$Sub/$Voice.onnx.json" "$Voice.onnx.json" }
    if (-not (Test-Path "$Voice.onnx")) { Get-File "$Base/$Sub/$Voice.onnx" "$Voice.onnx" }
}
Write-Output "Done. Graphit now reads aloud with natural voices."
