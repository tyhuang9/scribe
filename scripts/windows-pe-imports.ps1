$ReviewedWindowsSystemDlls = @(
    "advapi32.dll",
    "api-ms-win-core-path-l1-1-0.dll",
    "api-ms-win-core-synch-l1-2-0.dll",
    "api-ms-win-shcore-scaling-l1-1-1.dll",
    "bcrypt.dll",
    "bcryptprimitives.dll",
    "comctl32.dll",
    "dbghelp.dll",
    "dwmapi.dll",
    "dxgi.dll",
    "gdi32.dll",
    "gdiplus.dll",
    "imm32.dll",
    "kernel32.dll",
    "ntdll.dll",
    "ole32.dll",
    "oleaut32.dll",
    "opengl32.dll",
    "setupapi.dll",
    "shell32.dll",
    "shlwapi.dll",
    "uiautomationcore.dll",
    "user32.dll",
    "uxtheme.dll",
    "ws2_32.dll"
)

function Assert-PeByteRange(
    [byte[]]$Bytes,
    [long]$Offset,
    [long]$Length,
    [string]$Context
) {
    if ($Offset -lt 0 -or $Length -lt 0 -or $Offset -gt $Bytes.LongLength - $Length) {
        throw "PE $Context extends outside the file."
    }
}

function Read-PeUInt16([byte[]]$Bytes, [long]$Offset, [string]$Context) {
    Assert-PeByteRange $Bytes $Offset 2 $Context
    return [BitConverter]::ToUInt16($Bytes, [int]$Offset)
}

function Read-PeUInt32([byte[]]$Bytes, [long]$Offset, [string]$Context) {
    Assert-PeByteRange $Bytes $Offset 4 $Context
    return [BitConverter]::ToUInt32($Bytes, [int]$Offset)
}

function Read-PeUInt64([byte[]]$Bytes, [long]$Offset, [string]$Context) {
    Assert-PeByteRange $Bytes $Offset 8 $Context
    return [BitConverter]::ToUInt64($Bytes, [int]$Offset)
}

function Convert-PeRvaToFileOffset(
    [byte[]]$Bytes,
    [uint32]$Rva,
    [uint32]$Length,
    [uint32]$SizeOfHeaders,
    [object[]]$Sections,
    [string]$Context
) {
    $rvaStart = [uint64]$Rva
    $rvaEnd = $rvaStart + [uint64]$Length
    if ($rvaEnd -gt [uint64][uint32]::MaxValue + 1) {
        throw "PE $Context RVA overflows the 32-bit image address space."
    }

    if ($rvaStart -lt [uint64]$SizeOfHeaders) {
        if ($rvaEnd -gt [uint64]$SizeOfHeaders) {
            throw "PE $Context crosses the mapped header boundary."
        }
        Assert-PeByteRange $Bytes ([long]$rvaStart) ([long]$Length) $Context
        return [long]$rvaStart
    }

    $matches = [System.Collections.Generic.List[object]]::new()
    foreach ($section in $Sections) {
        $sectionStart = [uint64]$section.VirtualAddress
        $sectionSpan = [Math]::Max([uint64]$section.VirtualSize, [uint64]$section.RawSize)
        $sectionEnd = $sectionStart + $sectionSpan
        if ($rvaStart -ge $sectionStart -and $rvaStart -lt $sectionEnd) {
            $delta = $rvaStart - $sectionStart
            if ($delta + [uint64]$Length -gt [uint64]$section.RawSize) {
                throw "PE $Context maps into an unbacked virtual section tail."
            }
            $fileOffset = [uint64]$section.RawPointer + $delta
            Assert-PeByteRange $Bytes ([long]$fileOffset) ([long]$Length) $Context
            $matches.Add([long]$fileOffset)
        }
    }
    if ($matches.Count -ne 1) {
        throw "PE $Context RVA must map to exactly one section; found $($matches.Count)."
    }
    return [long]$matches[0]
}

function Read-PeAsciiDllName(
    [byte[]]$Bytes,
    [uint32]$Rva,
    [uint32]$SizeOfHeaders,
    [object[]]$Sections,
    [string]$Context
) {
    if ($Rva -eq 0) {
        throw "PE $Context has a null DLL name RVA."
    }
    $nameBytes = [System.Collections.Generic.List[byte]]::new()
    $terminated = $false
    for ($index = 0; $index -lt 260; $index++) {
        $currentRva64 = [uint64]$Rva + [uint64]$index
        if ($currentRva64 -gt [uint64][uint32]::MaxValue) {
            throw "PE $Context DLL name RVA overflows."
        }
        $offset = Convert-PeRvaToFileOffset `
            $Bytes ([uint32]$currentRva64) 1 $SizeOfHeaders $Sections "$Context DLL name"
        $value = $Bytes[[int]$offset]
        if ($value -eq 0) {
            $terminated = $true
            break
        }
        if ($value -lt 0x21 -or $value -gt 0x7E) {
            throw "PE $Context DLL name is not canonical printable ASCII."
        }
        $nameBytes.Add($value)
    }
    if (-not $terminated -or $nameBytes.Count -eq 0) {
        throw "PE $Context DLL name is missing or not null-terminated within 260 bytes."
    }
    $name = [System.Text.Encoding]::ASCII.GetString($nameBytes.ToArray())
    if ($name -notmatch '^[A-Za-z0-9._-]+\.dll$' -or
        $name.Contains('\') -or $name.Contains('/') -or $name.Contains(':')) {
        throw "PE $Context contains a non-canonical DLL import name: $name"
    }
    return $name.ToLowerInvariant()
}

function Read-PeImportDirectory(
    [byte[]]$Bytes,
    [uint32]$DirectoryRva,
    [uint32]$DirectorySize,
    [uint32]$SizeOfHeaders,
    [object[]]$Sections,
    [ValidateSet("normal", "delay")]
    [string]$Kind
) {
    if ($DirectoryRva -eq 0 -and $DirectorySize -eq 0) {
        return @()
    }
    if ($DirectoryRva -eq 0 -or $DirectorySize -eq 0) {
        throw "PE $Kind import directory has an inconsistent RVA/size pair."
    }
    $descriptorSize = if ($Kind -eq "normal") { 20 } else { 32 }
    if ($DirectorySize -lt $descriptorSize) {
        throw "PE $Kind import directory is smaller than one descriptor."
    }
    $directoryOffset = Convert-PeRvaToFileOffset `
        $Bytes $DirectoryRva $DirectorySize $SizeOfHeaders $Sections "$Kind import directory"
    $directoryEnd = $directoryOffset + [long]$DirectorySize
    $cursor = $directoryOffset
    $terminated = $false
    $names = [System.Collections.Generic.List[string]]::new()
    $nameSet = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )

    while ($cursor + $descriptorSize -le $directoryEnd) {
        if ($Kind -eq "normal") {
            $fields = @(
                Read-PeUInt32 $Bytes $cursor "$Kind import descriptor"
                Read-PeUInt32 $Bytes ($cursor + 4) "$Kind import descriptor"
                Read-PeUInt32 $Bytes ($cursor + 8) "$Kind import descriptor"
                Read-PeUInt32 $Bytes ($cursor + 12) "$Kind import descriptor"
                Read-PeUInt32 $Bytes ($cursor + 16) "$Kind import descriptor"
            )
            $nameRva = [uint32]$fields[3]
        }
        else {
            $fields = @(
                Read-PeUInt32 $Bytes $cursor "$Kind import descriptor"
                Read-PeUInt32 $Bytes ($cursor + 4) "$Kind import descriptor"
                Read-PeUInt32 $Bytes ($cursor + 8) "$Kind import descriptor"
                Read-PeUInt32 $Bytes ($cursor + 12) "$Kind import descriptor"
                Read-PeUInt32 $Bytes ($cursor + 16) "$Kind import descriptor"
                Read-PeUInt32 $Bytes ($cursor + 20) "$Kind import descriptor"
                Read-PeUInt32 $Bytes ($cursor + 24) "$Kind import descriptor"
                Read-PeUInt32 $Bytes ($cursor + 28) "$Kind import descriptor"
            )
            if (@($fields | Where-Object { $_ -ne 0 }).Count -gt 0 -and [uint32]$fields[0] -ne 1) {
                throw "PE delay import descriptor must use RVA-based attributes."
            }
            $nameRva = [uint32]$fields[1]
        }

        if (@($fields | Where-Object { $_ -ne 0 }).Count -eq 0) {
            $terminated = $true
            break
        }
        $name = Read-PeAsciiDllName $Bytes $nameRva $SizeOfHeaders $Sections "$Kind import"
        if ($nameSet.Add($name)) {
            $names.Add($name)
        }
        $cursor += $descriptorSize
    }
    if (-not $terminated) {
        throw "PE $Kind import directory is missing its null descriptor terminator."
    }
    return @($names | Sort-Object)
}

function Get-WindowsPeImportReport([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "PE dependency input is missing: $Path"
    }
    $item = Get-Item -LiteralPath $Path -Force
    if (($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "PE dependency input cannot be a symbolic link or reparse point: $Path"
    }
    $bytes = [System.IO.File]::ReadAllBytes($item.FullName)
    if ($bytes.LongLength -lt 256 -or $bytes[0] -ne 0x4D -or $bytes[1] -ne 0x5A) {
        throw "PE dependency input has an invalid DOS header: $Path"
    }
    $peOffset = [long](Read-PeUInt32 $bytes 0x3C "header offset")
    if ((Read-PeUInt32 $bytes $peOffset "signature") -ne 0x00004550) {
        throw "PE dependency input has an invalid signature: $Path"
    }
    $coffOffset = $peOffset + 4
    $machine = Read-PeUInt16 $bytes $coffOffset "COFF header"
    $sectionCount = Read-PeUInt16 $bytes ($coffOffset + 2) "COFF section count"
    $optionalSize = Read-PeUInt16 $bytes ($coffOffset + 16) "COFF optional-header size"
    if ($sectionCount -lt 1 -or $sectionCount -gt 96 -or $optionalSize -lt 224) {
        throw "PE dependency input has invalid section or optional-header bounds: $Path"
    }
    $optionalOffset = $coffOffset + 20
    Assert-PeByteRange $bytes $optionalOffset $optionalSize "optional header"
    $magic = Read-PeUInt16 $bytes $optionalOffset "optional-header magic"
    if ($magic -ne 0x20B) {
        throw "PE dependency input must be PE32+; got optional magic 0x$($magic.ToString('x'))."
    }
    $subsystem = Read-PeUInt16 $bytes ($optionalOffset + 68) "subsystem"
    $sizeOfHeaders = Read-PeUInt32 $bytes ($optionalOffset + 60) "SizeOfHeaders"
    if ($sizeOfHeaders -eq 0 -or $sizeOfHeaders -gt $bytes.LongLength) {
        throw "PE dependency input has invalid SizeOfHeaders."
    }
    $directoryCount = Read-PeUInt32 $bytes ($optionalOffset + 108) "data-directory count"
    if ($directoryCount -lt 14) {
        throw "PE dependency input does not expose both normal and delay import directories."
    }

    $sectionTable = $optionalOffset + $optionalSize
    Assert-PeByteRange $bytes $sectionTable ([long]$sectionCount * 40) "section table"
    $sections = [System.Collections.Generic.List[object]]::new()
    for ($index = 0; $index -lt $sectionCount; $index++) {
        $sectionOffset = $sectionTable + ($index * 40)
        $virtualSize = Read-PeUInt32 $bytes ($sectionOffset + 8) "section virtual size"
        $virtualAddress = Read-PeUInt32 $bytes ($sectionOffset + 12) "section RVA"
        $rawSize = Read-PeUInt32 $bytes ($sectionOffset + 16) "section raw size"
        $rawPointer = Read-PeUInt32 $bytes ($sectionOffset + 20) "section raw pointer"
        if ($rawSize -gt 0) {
            Assert-PeByteRange $bytes $rawPointer $rawSize "section raw data"
        }
        $sections.Add([pscustomobject]@{
            VirtualSize = [uint32]$virtualSize
            VirtualAddress = [uint32]$virtualAddress
            RawSize = [uint32]$rawSize
            RawPointer = [uint32]$rawPointer
        })
    }

    $normalDirectoryOffset = $optionalOffset + 112 + 8
    $delayDirectoryOffset = $optionalOffset + 112 + (13 * 8)
    $normalRva = Read-PeUInt32 $bytes $normalDirectoryOffset "normal import RVA"
    $normalSize = Read-PeUInt32 $bytes ($normalDirectoryOffset + 4) "normal import size"
    $delayRva = Read-PeUInt32 $bytes $delayDirectoryOffset "delay import RVA"
    $delaySize = Read-PeUInt32 $bytes ($delayDirectoryOffset + 4) "delay import size"
    $normalImports = @(Read-PeImportDirectory `
        $bytes $normalRva $normalSize $sizeOfHeaders $sections.ToArray() "normal")
    $delayImports = @(Read-PeImportDirectory `
        $bytes $delayRva $delaySize $sizeOfHeaders $sections.ToArray() "delay")

    return [pscustomobject]@{
        Path = $item.FullName
        Machine = [uint16]$machine
        Subsystem = [uint16]$subsystem
        NormalImports = $normalImports
        DelayImports = $delayImports
    }
}

function Assert-ReviewedWindowsPe([string]$Path) {
    $report = Get-WindowsPeImportReport $Path
    if ($report.Machine -ne 0x8664) {
        throw "PE Machine mismatch for ${Path}: expected AMD64 (0x8664), got 0x$($report.Machine.ToString('x4'))."
    }
    if ($report.Subsystem -ne 2) {
        throw "PE subsystem mismatch for ${Path}: expected Windows GUI (2), got $($report.Subsystem)."
    }
    if ($report.NormalImports.Count -eq 0) {
        throw "PE normal import closure is unexpectedly empty for $Path."
    }
    $allowlist = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    foreach ($allowedDll in $ReviewedWindowsSystemDlls) {
        $null = $allowlist.Add($allowedDll)
    }
    foreach ($import in $report.NormalImports) {
        if (-not $allowlist.Contains($import)) {
            throw "PE contains an unreviewed normal import DLL: $import"
        }
    }
    foreach ($import in $report.DelayImports) {
        if (-not $allowlist.Contains($import)) {
            throw "PE contains an unreviewed delay import DLL: $import"
        }
    }
    return $report
}
