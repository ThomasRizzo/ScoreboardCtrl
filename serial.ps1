[System.IO.Ports.SerialPort]::GetPortNames()

<# 
38400,N,8,1

Packet from scoreboard is 6 bytes:
    0x00
    Minutes
    Seconds
    Shotclock
    0x3F
    CRC?
#>

if ($s) { $s.Close() }
$s = New-Object System.IO.Ports.SerialPort COM26, 38400, None, 8, one; $s.Open()
$s.ReadExisting() | Out-Null
while ($true) { 
    $x = Get-Date
    $b = $s.ReadByte() 
    $stx = if ((Get-Date).Subtract($x).TotalMilliseconds -gt 10) { Clear-Host; "`nSTART`n" }
    "{1}0x{0:X} b{0:B} {0} {2}" -f $b, $stx, (0xFF - $b -shr 1)
}
