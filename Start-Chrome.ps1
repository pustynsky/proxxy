$ErrorActionPreference = 'Stop'

try {
    foreach ($process in @(Get-Process -Name chrome -ErrorAction SilentlyContinue)) {
        Stop-Process -Id $process.Id -Force -ErrorAction Stop
        $process.WaitForExit()
    }

    $candidates = @(
        "$env:ProgramFiles\Google\Chrome\Application\chrome.exe"
        "${env:ProgramFiles(x86)}\Google\Chrome\Application\chrome.exe"
        "$env:LOCALAPPDATA\Google\Chrome\Application\chrome.exe"
    )
    $chrome = $candidates | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    if (-not $chrome) {
        throw 'Google Chrome was not found in its standard installation directories.'
    }

    $exe = Join-Path $PSScriptRoot 'target\release\proxxy.exe'
    if (-not (Test-Path -LiteralPath $exe)) {
        throw 'Build the proxy first with: cargo build --release'
    }

    $listeners = @(Get-NetTCPConnection -State Listen -ErrorAction Stop |
        Where-Object { $_.LocalPort -eq 8899 })
    if ($listeners.Count -gt 0) {
        foreach ($listener in $listeners) {
            $owner = Get-Process -Id $listener.OwningProcess -ErrorAction Stop
            if ($owner.Path -ne $exe -or $listener.LocalAddress -ne '127.0.0.1') {
                throw 'Port 8899 is occupied by another listener. Stop it before launching.'
            }
        }
        $proxy = $owner
    } else {
        $proxy = Start-Process -FilePath $exe -NoNewWindow -PassThru
    }

    try {
        if ($listeners.Count -eq 0) {
            $ready = $false
            for ($attempt = 0; $attempt -lt 50; $attempt++) {
                Start-Sleep -Milliseconds 100
                $proxy.Refresh()
                if ($proxy.HasExited) {
                    throw 'Proxxy exited during startup. Check its console output.'
                }
                $connection = New-Object System.Net.Sockets.TcpClient
                try {
                    $connection.Connect('127.0.0.1', 8899)
                    $ready = $true
                    break
                } catch [System.Net.Sockets.SocketException] {
                    if ($attempt -eq 49) { throw }
                } finally {
                    $connection.Dispose()
                }
            }
            if (-not $ready) { throw 'Proxxy did not become ready.' }
        }

        $browser = Start-Process -FilePath $chrome -ArgumentList @(
            '--proxy-server=https=127.0.0.1:8899'
            '--disable-background-mode'
        ) -PassThru
        Write-Host 'Chrome launched with the HTTPS proxy. Windows settings were not changed.'
        Write-Host 'Keep this launcher open. Closing the last Chrome window will stop Proxxy too.'
        Write-Host 'To return to normal: close Chrome and launch it using your usual shortcut.'
        $browser.WaitForExit()
    } finally {
        $proxy.Refresh()
        if (-not $proxy.HasExited) {
            Stop-Process -Id $proxy.Id -ErrorAction Stop
        }
    }
} catch {
    Write-Error $_ -ErrorAction Continue
    exit 1
}
