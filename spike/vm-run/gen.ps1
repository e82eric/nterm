1..200 | ForEach-Object { Write-Host "`e[3$($_ % 8)mhistory line $_`e[0m" }
