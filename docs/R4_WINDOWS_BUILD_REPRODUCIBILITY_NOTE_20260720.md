# AgoraLink R4 Windows Build Reproducibility Note

Date: 2026-07-20

## Conclusion

`WINDOWS_MSVC_DEBUG_BUILD_BYTE_REPRODUCIBILITY_NOT_ESTABLISHED`

The rebuilt executable is not byte-for-byte identical to the frozen R4 executable, but the observed differences are limited to Windows linker/debug metadata. This comparison found no source-tree change and no functional code-section difference. It does not support a `SOURCE_MISMATCH` conclusion.

The v0.0.11 release asset remains the already validated frozen portable whose bundled native executable has SHA-256 `D0CEE722185FF1E294894557C008CA96B37FF8C75F909162DB7B658286C8AE9D`. The temporary rebuilt executable must not replace it.

## Inputs

Frozen R4 executable:

`rust-native/agoralink_media/target/release/agoralink_media.exe`

- Size: 2,143,744 bytes
- SHA-256: `D0CEE722185FF1E294894557C008CA96B37FF8C75F909162DB7B658286C8AE9D`

Temporary rebuilt executable:

`_local_artifacts/R4_GITHUB_RELEASE_BLOCKED_20260720_20260720_131240/agoralink_media.rebuilt_hash_changed.exe`

- Size: 2,143,744 bytes
- SHA-256: `99D58D69DB31FF32F862B0D100E07DF7AE7C677B1E71A6D1DB5F6EC65272F1D1`

Frozen PDB SHA-256:

`0761475AD6D852B4A55A02D05C51706FCC838F9F0BBDE2FB5D40135EA32ECC2E`

Temporary rebuilt PDB SHA-256:

`724D51220F75AF45E5911D3EEBDA86567F68E8595959A1E56FD06B0175417562`

## Toolchain

- Rust: `rustc 1.96.0 (ac68faa20 2026-05-25)`
- Rust host: `x86_64-pc-windows-msvc`
- LLVM reported by rustc: `22.1.2`
- Cargo: `cargo 1.96.0 (30a34c682 2026-05-25)`
- Microsoft linker/dumpbin: Visual Studio 2022 MSVC `14.44.35207`, linker version `14.44`
- `llvm-readobj` and `llvm-objdump` were not installed on this host; `dumpbin` plus a direct PE parser were used.

## Source Provenance

The frozen R4 artifact was retained as the validated release input. From regression-evidence commit `ad23a3c2daf08868afd2784fcb4b8e476458ce64` to GUI portable commit `6181f71eb2438bc5c56dd3540de5abfe995848b6`:

- Rust `src` tree ID at both commits: `f4a2399b5d60c79a60ff1818b39b72e1cb799d42`
- `Cargo.lock` blob ID at both commits: `90f71cae3ddbcf6b8020f45912ba46b8230c3110`
- Changed Rust core files: 0

The comparison used the same checked-in source tree and lockfile. No Rust product source was modified for this diagnosis.

## PE Structural Comparison

Both executables have the same:

- File size: 2,143,744 bytes
- Machine: x64 (`0x8664`)
- Section count: 5
- Optional-header size: 240 bytes
- Linker version: 14.44
- Size of code: 1,586,688 bytes
- Size of initialized data: 556,544 bytes
- Entry point RVA: `0x17D1FC`
- Image base: `0x140000000`
- Image size: `0x20F000`
- Import table: identical, 371 normalized lines
- Section names, RVAs, raw offsets, and raw sizes

Section-level SHA-256 comparison:

| Section | Layout equal | Content equal | Result |
|---|---:|---:|---|
| `.text` | yes | yes | Functional code bytes are identical |
| `.rdata` | yes | no | Link/debug metadata differences only |
| `.data` | yes | yes | Identical |
| `.pdata` | yes | yes | Identical unwind metadata |
| `.reloc` | yes | yes | Identical relocations |

Only 28 bytes differ across the complete PE files:

- 3 bytes in the PE file-header timestamp.
- Three 3-byte differences in `.rdata`, corresponding to debug-directory timestamps.
- 16 bytes in `.rdata`, corresponding to the CodeView RSDS GUID.

`dumpbin /headers` confirms:

- Frozen timestamp: `6A59DE7A` (2026-07-17 15:49:14 local display from dumpbin)
- Rebuilt timestamp: `6A5DADE1` (2026-07-20 13:10:57 local display from dumpbin)
- Frozen RSDS GUID: `{24F83E43-212C-44B3-817C-D250857EB121}`
- Rebuilt RSDS GUID: `{ACA93669-BFAE-4ECE-A6D7-26C75AC20B9D}`
- PDB basename in both files: `agoralink_media.pdb`

No unexplained `.text`, import, data, unwind, relocation, entry-point, image-layout, or section-layout difference was found.

## Gate Interpretation

The Windows MSVC `release + debuginfo` output is not currently proven byte reproducible because its PE/PDB link metadata changes on relink. Release acceptance therefore uses these independent gates:

1. The frozen portable ZIP must remain SHA-256 `41CBAEE0ADC3F0F542811D783A7B84B66F1A011C56FD4F82D39FAEDD8B18E942`.
2. Its bundled native executable must remain the validated D0 artifact.
3. The portable must contain no R3 binary.
4. Rust release tests, independent release build, and independent self-test must pass.
5. GUI configuration/command tests and portable independent-launch evidence must pass.
6. The independent rebuild must not be copied into the frozen portable.

Future byte-reproducible-build work may investigate `/Brepro`, stable PDB identity/path handling, `SOURCE_DATE_EPOCH`, pinned linker metadata, and clean CI builds. Those changes are outside v0.0.11 and require separate validation.

## Evidence

Detailed local evidence is under:

`_local_artifacts/R4_WINDOWS_BUILD_REPRO_20260720`

It contains `dumpbin` headers/all/imports, normalized import comparison, direct PE section hashes, byte-difference ranges, toolchain versions, and source-tree comparison data.
