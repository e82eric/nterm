# Turn the mouse wheel at a screen position. Positive notches scroll up.
param([int]$X, [int]$Y, [int]$Notches)
Add-Type -Namespace Vm -Name Mouse -MemberDefinition @"
[DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
[DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, int data, System.UIntPtr extra);
"@
[Vm.Mouse]::SetCursorPos($X, $Y) | Out-Null
Start-Sleep -Milliseconds 200
$step = if ($Notches -ge 0) { 120 } else { -120 }
for ($i = 0; $i -lt [Math]::Abs($Notches); $i++) {
    [Vm.Mouse]::mouse_event(0x0800, 0, 0, $step, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 60
}
