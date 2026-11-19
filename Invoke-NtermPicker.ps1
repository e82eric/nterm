# Kept for profiles and scripts that dot-source it from here. The function is
# part of the NTerm PowerShell module now: powershell\NTerm, installed by
# build.ps1 as share\nterm\powershell\NTerm. See profile.example.ps1.
. (Join-Path $PSScriptRoot 'powershell\NTerm\Invoke-NtermPicker.ps1') @args
