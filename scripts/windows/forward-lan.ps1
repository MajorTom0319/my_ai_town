param(
    [Parameter(Mandatory = $true)][System.Net.IPAddress]$ListenAddress,
    [Parameter(Mandatory = $true)][System.Net.IPAddress]$WslAddress,
    [ValidateRange(1, 65535)][int]$Port = 5174,
    [string]$TrustedSubnet,
    [switch]$Remove
)

$ErrorActionPreference = 'Stop'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    throw 'Run this script in an administrator PowerShell.'
}
foreach ($address in @($ListenAddress, $WslAddress)) {
    if ($address.AddressFamily -ne [System.Net.Sockets.AddressFamily]::InterNetwork -or
        $address.Equals([System.Net.IPAddress]::Any) -or
        [System.Net.IPAddress]::IsLoopback($address)) {
        throw 'Use specific non-loopback IPv4 addresses.'
    }
}
$ruleName = "AiTown-LAN-$ListenAddress-$Port"
if ($Remove) {
    & netsh interface portproxy delete v4tov4 "listenaddress=$ListenAddress" "listenport=$Port"
    if ($LASTEXITCODE -ne 0) { throw 'Failed to remove port forwarding.' }
    Get-NetFirewallRule -Name $ruleName -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    return
}
$interface = Get-NetIPAddress -IPAddress "$ListenAddress" -ErrorAction SilentlyContinue
if (-not $interface) {
    throw 'ListenAddress must be an IPv4 address on this Windows computer.'
}
if ($TrustedSubnet -notmatch '^([0-9]{1,3}\.){3}[0-9]{1,3}/([0-9]|[12][0-9]|3[0-2])$') {
    throw 'TrustedSubnet must be an explicit trusted IPv4 CIDR, for example 172.31.104.0/23.'
}
$null = [System.Net.IPAddress]::Parse($TrustedSubnet.Split('/')[0])
if (-not (Get-NetConnectionProfile -InterfaceIndex $interface.InterfaceIndex | Where-Object NetworkCategory -eq Private)) {
    throw 'The listening interface must already be a trusted Private network.'
}
if (-not (Get-NetFirewallProfile -Name Private).Enabled) {
    throw 'Enable the Private network firewall before sharing.'
}
Start-Service iphlpsvc
& netsh interface portproxy add v4tov4 "listenaddress=$ListenAddress" "listenport=$Port" "connectaddress=$WslAddress" "connectport=$Port"
if ($LASTEXITCODE -ne 0) { throw 'Failed to configure port forwarding.' }
Get-NetFirewallRule -Name $ruleName -ErrorAction SilentlyContinue | Remove-NetFirewallRule
New-NetFirewallRule -Name $ruleName -DisplayName "AI Town LAN ($ListenAddress`:$Port)" -Direction Inbound -Action Allow -Protocol TCP -LocalAddress "$ListenAddress" -LocalPort $Port -RemoteAddress $TrustedSubnet -InterfaceAlias $interface.InterfaceAlias -Profile Private | Out-Null
Write-Output "LAN URL: http://$ListenAddress`:$Port/"
