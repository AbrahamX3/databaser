# Rasterises assets/icon.ico into the raw RGBA the tray icon needs.
#
# `gpui-tray` takes non-premultiplied RGBA8 pixels, and the app has no image
# decoder to produce them at runtime, so the 32x32 frame is baked once and
# committed. Re-run this only if assets/icon.ico changes.
#
#   powershell -File scripts/make-tray-icon.ps1 -Size 32 -OutputPath assets/tray-32.rgba

param(
    [int]$Size = 32,
    [string]$IconPath = "assets/icon.ico",
    [string]$OutputPath = "assets/tray-32.rgba"
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

$resolved = (Resolve-Path $IconPath).Path
$icon = New-Object System.Drawing.Icon($resolved, $Size, $Size)
$bitmap = $icon.ToBitmap()

try {
    if ($bitmap.Width -ne $Size -or $bitmap.Height -ne $Size) {
        throw "expected a ${Size}x${Size} frame, got $($bitmap.Width)x$($bitmap.Height)"
    }

    $rect = New-Object System.Drawing.Rectangle(0, 0, $Size, $Size)
    $locked = $bitmap.LockBits(
        $rect,
        [System.Drawing.Imaging.ImageLockMode]::ReadOnly,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
    )

    try {
        $bytes = New-Object byte[] ($locked.Stride * $Size)
        [System.Runtime.InteropServices.Marshal]::Copy($locked.Scan0, $bytes, 0, $bytes.Length)
    }
    finally {
        $bitmap.UnlockBits($locked)
    }

    # GDI+ stores 32bppArgb as BGRA; the tray backend wants RGBA.
    $rgba = New-Object byte[] ($Size * $Size * 4)
    for ($y = 0; $y -lt $Size; $y++) {
        for ($x = 0; $x -lt $Size; $x++) {
            $src = $y * $locked.Stride + $x * 4
            $dst = ($y * $Size + $x) * 4
            $rgba[$dst] = $bytes[$src + 2]
            $rgba[$dst + 1] = $bytes[$src + 1]
            $rgba[$dst + 2] = $bytes[$src]
            $rgba[$dst + 3] = $bytes[$src + 3]
        }
    }

    $directory = Split-Path -Parent $OutputPath
    if ($directory -and -not (Test-Path $directory)) {
        New-Item -ItemType Directory -Force $directory | Out-Null
    }
    [System.IO.File]::WriteAllBytes($OutputPath, $rgba)
    Write-Output "Wrote $OutputPath ($($rgba.Length) bytes)"
}
finally {
    $bitmap.Dispose()
    $icon.Dispose()
}
