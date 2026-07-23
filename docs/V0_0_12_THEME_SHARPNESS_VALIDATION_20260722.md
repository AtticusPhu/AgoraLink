# AgoraLink v0.0.12 Theme and Sharpness Validation

Date: 2026-07-22

Branch: `ui-theme-sharpness-v0.0.12`

Base: `aa1ef7f991129a627165b0dbe45259b22e6f8180`

Validation scope: uncommitted source worktree

## Automated Results

| Gate | Result |
|---|---|
| Pixel-Snap A/B | PASS, 41/41 fractional -> 0/41 |
| Python `py_compile` | PASS |
| Python unit tests | PASS, 103/103 |
| `screen_runtime.py --self-test` | PASS, 18/18 |
| `pip check` | PASS, no broken requirements |
| PowerShell parse | PASS, 23 files, 0 errors |
| `git diff --check` | PASS; line-ending warnings only |
| Rust tests | NOT_RERUN_UNCHANGED_RUST |
| CI | NOT_RUN_LOCAL_ONLY |

The Kivy test log reports missing optional `win32file` support in the file chooser, but no test fails and no theme path depends on it.

## Behavioral Coverage

- Default/missing/invalid/legacy theme configuration.
- Immediate Light/Dark switch and persistence.
- Toolbar/Settings selector synchronization.
- Existing and newly created widgets.
- Window background, primary button, text input, message bubble, file card, screen card, Settings page, Contact popup, and Confirmation dialog.
- Context menu and Spinner dropdown close on switch.
- Focus and disabled state preservation.
- Language, current conversation/group, draft, settings section, and unsaved non-theme values remain intact.
- Theme switching does not call worker stop/restart paths.
- Popup resize callback and context-menu resize callback are unbound on close.
- Integer geometry at 1280, 1599, 1600, 1601 and simulated density 1.25/1.5.

## Visual Matrix

Accepted evidence: `_local_artifacts/V0_0_12_THEME_SHARPNESS_FIX/after_final_source_v2/`

| Metric | Value |
|---|---:|
| PNG screenshots | 70 |
| Light / Dark | 35 / 35 |
| Chinese / English | 38 / 32 |
| Fractional geometry cases | 0 |
| Client-size mismatches | 0 |
| Missing images | 0 |

Resolutions: 1280x720, 1366x768, 1599x900, 1600x900, 1601x900, 1920x1080.

The matrix covers General/Network/Transfer/Screen/About, contact/group/file/screen/diagnostics details, Receive-only primary surface, context menu, confirmation, error, Spinner dropdown, Toast, message bubble, file card, and screen card in Light/Dark and Chinese/English. The primary page is explicitly **Receive only / 仅接受**, not chat.

The accepted capture backend is Win32 `BitBlt/GetDIBits`. Earlier Kivy OpenGL screenshots with non-4-byte-aligned width corruption are excluded from accepted evidence.

## Acceptance Status

| Requirement | Status |
|---|---|
| Full dynamic Light/Dark architecture | PASS |
| Toolbar toggle | PASS |
| Settings segmented selector | PASS |
| Existing widget restyle | PASS |
| Popup/Menu/Spinner/Toast strategy | PASS |
| Persistence | PASS |
| Language-theme independence | PASS |
| UI state preservation | PASS |
| Chinese 1280x720 Light/Dark | PASS |
| English 1280x720 Light/Dark | PASS |
| 1599/1600/1601 geometry and source sharpness | PASS |
| Actual Windows 100% | PASS |
| Actual Windows 125% | MANUAL_REQUIRED |
| Actual Windows 150% | MANUAL_REQUIRED |
| Source/portable visual parity | NOT_RUN |
| Formal portable | BLOCKED_PENDING_FINAL_COMMIT_AND_PUSH |

## Portable and Release Gate

No formal v0.0.12 portable was produced from the uncommitted worktree. Therefore these fields remain intentionally unclaimed:

- portable path and SHA-256;
- App/native executable SHA-256;
- portable FFmpeg/PDB/DMP/source counts;
- portable privacy scan;
- portable Light/Dark startup and persistence;
- portable WM_CLOSE and residual-process checks;
- final CI result.

The existing canonical portable is not deleted or replaced. Old ZIP review remains `REVIEW_REQUIRED`.

## Deferred Manual Checks

- Actual Windows 125% and 150% display scaling.
- Dual-host validation: `MANUAL_VALIDATION_DEFERRED_BY_USER`.
- Active-share restart: `MANUAL_VALIDATION_DEFERRED_BY_USER`.
- File-append validation: `MANUAL_VALIDATION_DEFERRED_BY_USER`.

## Overall

Source implementation and deterministic source validation pass. The release candidate remains incomplete until the UI work is committed/pushed, CI passes, and one portable is rebuilt and independently validated from that exact commit.

Final status: `V0_0_12_THEME_SHARPNESS_FIX_BLOCKED`
