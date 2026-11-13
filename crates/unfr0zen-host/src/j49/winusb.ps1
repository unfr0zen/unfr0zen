# Installs winusb.inf's WinUSB for the OMAP boot ROM (0451:d00e), elevated,
# with the USB flags its enumeration needs. The INF's catalog is signed with
# a certificate made for this computer, trusted as a root and a publisher,
# whose private key is deleted once the catalog is signed. Windows binds the
# driver whenever the ROM appears.
param([string]$Dir)
$ErrorActionPreference = "Stop"
$inf = Join-Path $Dir "unfr0zen-omap-rom.inf"
$cat = Join-Path $Dir "unfr0zen-omap-rom.cat"
$public = Join-Path $Dir "unfr0zen-driver.cer"
New-FileCatalog -Path $inf -CatalogFilePath $cat -CatalogVersion 2.0 | Out-Null
$cert = New-SelfSignedCertificate -Type CodeSigningCert -Subject "CN=unfr0zen USB driver ($env:COMPUTERNAME)" -CertStoreLocation Cert:\LocalMachine\My -NotAfter (Get-Date).AddYears(30)
try {
    Export-Certificate -Cert $cert -FilePath $public | Out-Null
    Import-Certificate -FilePath $public -CertStoreLocation Cert:\LocalMachine\Root | Out-Null
    Import-Certificate -FilePath $public -CertStoreLocation Cert:\LocalMachine\TrustedPublisher | Out-Null
    $signed = Set-AuthenticodeSignature -FilePath $cat -Certificate $cert -HashAlgorithm SHA256
} finally {
    Remove-Item "Cert:\LocalMachine\My\$($cert.Thumbprint)" -DeleteKey
}
if ($signed.Status -ne "Valid") { throw "signing the catalog: $($signed.StatusMessage)" }
# The ROM answers neither the BOS nor the Microsoft OS descriptor request
# Windows makes while enumerating it: the BOS one fails enumeration (code
# 43), and both spend its 300 ms wait for the host.
$flags = "HKLM:\SYSTEM\CurrentControlSet\Control\usbflags\0451D00E0000"
New-Item -Path $flags -Force | Out-Null
New-ItemProperty -Path $flags -Name SkipBOSDescriptorQuery -PropertyType DWord -Value 1 -Force | Out-Null
New-ItemProperty -Path $flags -Name osvc -PropertyType Binary -Value ([byte[]](0, 0)) -Force | Out-Null
& pnputil.exe /add-driver $inf /install | Out-Null
# 259: added, with no device present to install it on yet; 3010: reboot pending.
if ($LASTEXITCODE -in 0, 259, 3010) { exit 0 }
exit $LASTEXITCODE
