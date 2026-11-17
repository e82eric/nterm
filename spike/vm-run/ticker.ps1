# A stand-in for a full-screen program with an input box: full-width rules,
# the cursor parked in the box, and a status row rewritten with relative
# cursor moves. If a terminal's cursor is a row off, the ticks land on the
# wrong row.
$rule = [string]::new([char]0x2500, [Console]::WindowWidth)
[Console]::Write("$rule`n> input box`n$rule`n  status line   tick 0`n")
[Console]::Write("`e[3A`e[3G")
$n = 0
while ($true) {
    $n++
    [Console]::Write("`e[2B`e[18Gtick $n `e[2A`e[3G")
    Start-Sleep -Milliseconds 300
}
