# Authenticode-sign one file through Azure Artifact Signing (ADR-0093, #227).
# Tauri's bundle.windows.signCommand calls this for each binary it packages;
# windows-installer.yml calls it for the sidecars. It runs only after compilation, in a
# step whose environment holds the Azure service principal (the dlib signs in
# through EnvironmentCredential), and reads the tool paths the "Fetch Artifact
# Signing client" step wrote.
param([Parameter(Mandatory = $true)][string] $Path)
$ErrorActionPreference = 'Stop'
foreach ($name in @('ARTIFACT_SIGNING_SIGNTOOL', 'ARTIFACT_SIGNING_DLIB', 'ARTIFACT_SIGNING_METADATA')) {
  $value = [Environment]::GetEnvironmentVariable($name)
  if (-not $value -or -not (Test-Path -LiteralPath $value)) { throw "$name does not name an existing file" }
}
if (-not (Test-Path -LiteralPath $Path)) { throw "Nothing to sign at $Path" }
# SHA-256 file and timestamp digests, timestamped by Microsoft's RFC 3161
# service so the signature outlives the short-lived certificate.
& $env:ARTIFACT_SIGNING_SIGNTOOL sign /v /fd SHA256 /tr 'http://timestamp.acs.microsoft.com' /td SHA256 `
  /dlib $env:ARTIFACT_SIGNING_DLIB /dmdf $env:ARTIFACT_SIGNING_METADATA $Path
if ($LASTEXITCODE -ne 0) { throw "signtool failed for $Path with exit code $LASTEXITCODE" }
