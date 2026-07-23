# AgoraLink v0.0.12 Sharpness Fix Report

Date: 2026-07-22
Status: **SOURCE PASS / PORTABLE COMPARISON PENDING**

## Finding

The settings blur was reproducible at Windows 96 DPI / 100% and was not isolated to a missing font, FBO, ScrollView, or portable manifest. The strongest source-level evidence was fractional geometry produced by proportional popup sizing, centered spacer tracks, and the 47/53 SettingRow split.

Representative pre-fix values included fractional popup/content placement and right-column widths such as `414.54` and `467.46` pixels. This caused small text, input borders, and separators to land between physical pixels.

## Controlled A/B

The A/B harness rendered the same settings widget, font, font size, copy, colors, controls, and ScrollView. The B case changed only popup extent/position, centered content geometry, SettingRow column boundaries, and rounded padding/spacing.

| Result | A: original | B: pixel snapped |
|---|---:|---:|
| Fractional geometry samples | 41 | 0 |
| Sample ratio | 1.000000 | 0.000000 |
| Font/color changes | none | none |

At 100% original-PNG inspection, B has steadier secondary text, borders, separators, and control-column alignment. Decision: `PIXEL_SNAPPING_CONFIRMED`. Pixel geometry is confirmed as a material contributor, not claimed as the only contributor.

Evidence:

- `_local_artifacts/V0_0_12_THEME_SHARPNESS_FIX/ab/ab_original.png`
- `_local_artifacts/V0_0_12_THEME_SHARPNESS_FIX/ab/ab_pixel_snapped.png`
- `_local_artifacts/V0_0_12_THEME_SHARPNESS_FIX/ab/ab_geometry_before.json`
- `_local_artifacts/V0_0_12_THEME_SHARPNESS_FIX/ab/ab_geometry_after.json`
- `_local_artifacts/V0_0_12_THEME_SHARPNESS_FIX/ab/ab_comparison.md`

The retained pre-fix folder contains nine source PNGs covering 1599/1600/1601 and multiple settings states. A matching pre-fix portable matrix was not regenerated in this worktree, so baseline source-versus-portable parity is not claimed.

## Geometry Changes

1. `ui_geometry.py` provides pure rounding, parity-aware centered extents, rectangle snapping, and exact integer width splitting.
2. `SecondaryPopup` now uses `size_hint=(None, None)`, fixed margins, bounded integer dimensions, and lifecycle-safe resize binding.
3. `PixelSnappedContentContainer` replaces symmetric proportional spacers and permits a harmless 1px left/right margin difference rather than a 0.5px content origin.
4. `SettingRow` uses fixed integer label/control widths; the widths plus the gap exactly equal available width.
5. The settings theme segmented control uses the same integer split helper.

The product still uses ScrollView. The fix does not mask blur by removing scrolling or globally enlarging text.

## Post-Fix Matrix

The accepted source matrix contains 70 lossless PNGs captured from the exact Win32 client area with `BitBlt/GetDIBits`, avoiding the OpenGL screenshot row-stride corruption found during evidence generation.

- Light: 35
- Dark: 35
- Chinese: 38
- English: 32
- Resolutions: 1280x720, 1366x768, 1599x900, 1600x900, 1601x900, 1920x1080
- Fractional geometry cases: 0
- Client-size mismatches: 0
- Missing images: 0
- Primary surface cases: 4, all marked `receive_only_not_chat`

Accepted root:

`_local_artifacts/V0_0_12_THEME_SHARPNESS_FIX/after_final_source_v2/`

The earlier `after/`, `after_verified/`, and first `after_final_source/` directories are not final evidence. Only `after_final_source_v2` and its SHA-256 manifest are accepted.

## Typography and Contrast

After the geometry gate passed, explanatory text was moved to integer font sizes in the 12-14sp range. Light muted text is `#667482`; Dark muted text is `#A3AFBA`. Automated tests enforce >= 4.5:1 muted-text contrast and >= 7.0:1 primary-text contrast against each theme surface.

## DPI Status

- Actual Windows 100% / 96 DPI: PASS.
- Simulated density 1.25 and 1.5 integer geometry: PASS through unit tests.
- Actual Windows 125%: `MANUAL_REQUIRED`.
- Actual Windows 150%: `MANUAL_REQUIRED`.

No registry or user display setting was changed by the automated validation.

## Remaining Boundary

Source-versus-portable comparison is pending because a formal portable must be built from the final pushed UI commit. No portable sharpness result or hash is claimed from the uncommitted working tree.
