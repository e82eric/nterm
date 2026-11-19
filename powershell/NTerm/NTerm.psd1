@{
    RootModule = 'NTerm.psm1'
    ModuleVersion = '0.1.0'
    GUID = '7876b3f2-fbce-46ee-99e5-ee748dc0f362'
    Author = 'nterm'
    Description = 'nterm for PowerShell: pickers for PSReadLine keys, and the prompt marks nterm reads.'
    PowerShellVersion = '7.4'
    FunctionsToExport = @(
        'Invoke-NtermPicker'
        'Disconnect-NtermPicker'
        'Enable-NtermShellIntegration'
        'Add-NtermPickedPath'
        'Set-NtermLocationFromPicker'
        'Set-NtermLineFromHistory'
        'Set-NtermLineFromRipgrep'
        'Complete-NtermLine'
    )
    CmdletsToExport = @()
    VariablesToExport = @()
    AliasesToExport = @()
}
