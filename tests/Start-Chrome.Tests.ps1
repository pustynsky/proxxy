$launcher = Join-Path (Split-Path $PSScriptRoot -Parent) 'Start-Chrome.ps1'
$proxyPath = Join-Path (Split-Path $PSScriptRoot -Parent) 'target\release\proxxy.exe'

function New-TestProcess {
    param([int]$ProcessId, [string]$Label)

    $process = [pscustomobject]@{
        Id = $ProcessId
        Path = $proxyPath
        Label = $Label
        HasExited = $false
        WaitError = $false
        Events = $global:proxxyLauncherTest.Events
    }
    $process | Add-Member ScriptMethod Refresh {}
    $process | Add-Member ScriptMethod WaitForExit {
        $this.Events.Add("$($this.Label):wait")
        if ($this.WaitError) { throw 'Browser wait failed.' }
        $this.HasExited = $true
    }
    $process
}

Describe 'Chrome launcher lifecycle' {
    BeforeEach {
        $global:proxxyLauncherTest = @{
            Events = New-Object 'System.Collections.Generic.List[string]'
            Errors = New-Object 'System.Collections.Generic.List[string]'
            PreviousBrowsers = @()
            Listeners = @()
            FailBrowserStart = $false
            ExitProxyWithBrowser = $false
            ProxyPath = $proxyPath
            Connection = [pscustomobject]@{}
        }
        $global:proxxyLauncherTest.Proxy = New-TestProcess -ProcessId 2002 -Label 'proxy'
        $global:proxxyLauncherTest.Browser = New-TestProcess -ProcessId 3003 -Label 'chrome'
        $global:proxxyLauncherTest.Connection | Add-Member ScriptMethod Connect {}
        $global:proxxyLauncherTest.Connection | Add-Member ScriptMethod Dispose {}

        Mock Get-Process {
            if ($Name -eq 'chrome') { return $global:proxxyLauncherTest.PreviousBrowsers }
            if ($Id -eq $global:proxxyLauncherTest.Proxy.Id) { return $global:proxxyLauncherTest.Proxy }
            throw "Unexpected process lookup: $Id"
        }
        Mock Get-NetTCPConnection { $global:proxxyLauncherTest.Listeners }
        Mock Test-Path { $true }
        Mock Start-Sleep {}
        Mock New-Object { $global:proxxyLauncherTest.Connection } -ParameterFilter {
            $TypeName -eq 'System.Net.Sockets.TcpClient'
        }
        Mock Start-Process {
            if ($FilePath -eq $global:proxxyLauncherTest.ProxyPath) {
                $global:proxxyLauncherTest.Events.Add('proxy:start')
                return $global:proxxyLauncherTest.Proxy
            }
            $global:proxxyLauncherTest.Events.Add('chrome:start')
            if ($global:proxxyLauncherTest.FailBrowserStart) { throw 'Browser startup failed.' }
            if ($global:proxxyLauncherTest.ExitProxyWithBrowser) {
                $global:proxxyLauncherTest.Proxy.HasExited = $true
            }
            $global:proxxyLauncherTest.Browser
        }
        Mock Stop-Process { $global:proxxyLauncherTest.Events.Add("stop:$Id") }
        Mock Write-Host {}
        Mock Write-Error { $global:proxxyLauncherTest.Errors.Add([string]$Message) }
    }

    AfterEach {
        Remove-Variable -Name proxxyLauncherTest -Scope Global
    }

    It 'waits for Chrome before stopping a newly started proxy' {
        & $launcher

        ($global:proxxyLauncherTest.Events -join ',') | Should Be 'proxy:start,chrome:start,chrome:wait,stop:2002'
        $global:proxxyLauncherTest.Errors.Count | Should Be 0
        Assert-MockCalled Start-Process -Times 1 -Exactly -Scope It -ParameterFilter {
            $FilePath -eq $proxyPath -and $NoNewWindow -and $PassThru
        }
        Assert-MockCalled Start-Process -Times 1 -Exactly -Scope It -ParameterFilter {
            $FilePath -ne $proxyPath -and $PassThru -and
            $ArgumentList -contains '--disable-background-mode' -and
            $ArgumentList -contains '--proxy-server=https=127.0.0.1:8899'
        }
    }

    It 'also stops a reused proxy after Chrome exits' {
        $global:proxxyLauncherTest.Listeners = @([pscustomobject]@{
            LocalPort = 8899
            LocalAddress = '127.0.0.1'
            OwningProcess = 2002
        })

        & $launcher

        ($global:proxxyLauncherTest.Events -join ',') | Should Be 'chrome:start,chrome:wait,stop:2002'
        $global:proxxyLauncherTest.Errors.Count | Should Be 0
    }

    It 'waits for old Chrome processes to exit before launching a new browser' {
        $global:proxxyLauncherTest.PreviousBrowsers = @(New-TestProcess -ProcessId 4004 -Label 'previous')

        & $launcher

        ($global:proxxyLauncherTest.Events -join ',') | Should Be 'stop:4004,previous:wait,proxy:start,chrome:start,chrome:wait,stop:2002'
        $global:proxxyLauncherTest.Errors.Count | Should Be 0
    }

    It 'stops the proxy and reports a Chrome startup failure' {
        $global:proxxyLauncherTest.FailBrowserStart = $true

        & $launcher

        ($global:proxxyLauncherTest.Events -join ',') | Should Be 'proxy:start,chrome:start,stop:2002'
        $global:proxxyLauncherTest.Errors.Count | Should Be 1
        $global:proxxyLauncherTest.Errors[0] | Should Match 'Browser startup failed'
        $LASTEXITCODE | Should Be 1
    }

    It 'stops the proxy and reports a Chrome wait failure' {
        $global:proxxyLauncherTest.Browser.WaitError = $true

        & $launcher

        ($global:proxxyLauncherTest.Events -join ',') | Should Be 'proxy:start,chrome:start,chrome:wait,stop:2002'
        $global:proxxyLauncherTest.Errors.Count | Should Be 1
        $global:proxxyLauncherTest.Errors[0] | Should Match 'Browser wait failed'
    }

    It 'does not try to stop a proxy that has already exited' {
        $global:proxxyLauncherTest.ExitProxyWithBrowser = $true

        & $launcher

        ($global:proxxyLauncherTest.Events -join ',') | Should Be 'proxy:start,chrome:start,chrome:wait'
        $global:proxxyLauncherTest.Errors.Count | Should Be 0
    }

    It 'reports early proxy exit without launching Chrome' {
        $global:proxxyLauncherTest.Proxy.HasExited = $true

        & $launcher

        ($global:proxxyLauncherTest.Events -join ',') | Should Be 'proxy:start'
        $global:proxxyLauncherTest.Errors.Count | Should Be 1
        $global:proxxyLauncherTest.Errors[0] | Should Match 'Proxxy exited during startup'
    }

    It 'does not stop a different application listening on the proxy port' {
        $global:proxxyLauncherTest.Proxy.Path = 'C:\Other\server.exe'
        $global:proxxyLauncherTest.Listeners = @([pscustomobject]@{
            LocalPort = 8899
            LocalAddress = '127.0.0.1'
            OwningProcess = 2002
        })

        & $launcher

        $global:proxxyLauncherTest.Events.Count | Should Be 0
        $global:proxxyLauncherTest.Errors.Count | Should Be 1
        $global:proxxyLauncherTest.Errors[0] | Should Match 'Port 8899 is occupied'
    }
}
