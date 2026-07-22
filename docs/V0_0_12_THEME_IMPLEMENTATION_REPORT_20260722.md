# AgoraLink v0.0.12 Theme Implementation Report

Date: 2026-07-22

Branch: `ui-theme-sharpness-v0.0.12`

Base commit: `aa1ef7f991129a627165b0dbe45259b22e6f8180`

Base tree: `1c290b5202980457194ff3ccfe2d5925f44156a4`

Final commit: `UNCOMMITTED_WORKTREE`

Source implementation: **PASS**

Release/portable gate: **BLOCKED_PENDING_FINAL_COMMIT_AND_PUSH**

## Scope

This change is limited to Kivy UI theme state, UI geometry, UI components, deterministic screenshot tooling, and tests. It does not modify Rust media, RUDP, chat/file-transfer protocols, databases, NACK/repair, worker lifecycle, or packaging logic.

The current primary product surface is treated as **Receive only / 仅接受**, not as a chat screen. The final fixture and screenshot metadata use `receive_only_not_chat`.

## Pixel-Snap Gate

The mandatory A/B experiment was completed before typography or color changes:

| Metric | Before | Pixel-snapped |
|---|---:|---:|
| Fractional samples | 41/41 | 0/41 |
| Fractional ratio | 1.000000 | 0.000000 |
| Font/font size changed | no | no |
| Colors changed | no | no |
| ScrollView removed | no | no |

Decision: `PIXEL_SNAPPING_CONFIRMED`. Fractional popup/content/column geometry is a material blur contributor. Low-contrast 11-12sp text was retained as a secondary contributor and adjusted only after this gate passed.

## Theme Architecture

`ui_theme_controller.py` now owns the process-wide observable theme state:

- Supported modes: `light`, `dark`; invalid and missing values normalize to `light`.
- Legacy `浅色` / `深色` values normalize to the corresponding mode.
- `theme_schema_version=2` and `theme_mode` are persisted without replacing unrelated GUI settings.
- Existing widgets register through weak references and receive idempotent `apply_theme()` calls.
- Background-thread requests are marshalled through `Clock.schedule_once`.
- `Window.clearcolor` and existing widgets update immediately.
- Destroyed widgets are not retained.

`ui_theme.py` defines `PRODUCT_LIGHT_THEME` and `PRODUCT_DARK_THEME` with matching semantic tokens. Compatibility aliases remain inside `ui_theme.py`; product modules no longer import a fixed secondary theme or capture a module-level palette.

## Main Tokens

| Token | Light | Dark |
|---|---|---|
| background | `#F5F7FA` | `#14181C` |
| surface | `#FFFFFF` | `#191E23` |
| surface_muted | `#EEF2F6` | `#20262C` |
| input_bg | `#EEF2F6` | `#20262C` |
| text_primary | `#20242A` | `#F0F3F6` |
| text_secondary | `#5F6B7A` | `#C2CBD4` |
| text_muted | `#667482` | `#A3AFBA` |
| border | `#D8E0EA` | `#343D46` |
| accent | `#3F7FA8` | `#3A82C3` |
| danger | `#B64B4B` | `#E66D6D` |

Automated contrast gates require primary text >= 7.0:1 and muted text >= 4.5:1 against the theme surface; both themes pass.

## Geometry

- `SecondaryPopup` uses fixed pixel extents, 24px margins, parity-aware centering, resize recalculation, and explicit unbinding on dismiss.
- `PixelSnappedContentContainer` centers one fixed-width content child without two fractional spacer tracks.
- `SettingRow` replaces the 47/53 `size_hint_x` split with exact integer columns and an 18px gap.
- The Light/Dark segmented control also uses integer width splitting at odd window widths.
- Geometry tests cover 1599/1600/1601 widths and simulated density 1.25/1.5.

## Theme Controls

- The primary toolbar has a one-click Light/Dark target button next to Settings.
- Settings > General has an immediate `Light | Dark` segmented selector.
- Both controls observe the same controller and remain synchronized.
- Theme changes persist immediately; Settings Cancel does not revert the global theme.
- Theme switching preserves language, active contact/group, draft text, settings section, unsaved non-theme values, transfer/screen state, and worker state.

## Surface Coverage

Live restyling is applied to window/root surfaces, toolbar/sidebar/content, settings shell/header/sidebar/footer, labels, inputs, buttons, message bubbles, file cards, screen cards, status/progress components, contact/group/detail pages, popups, confirmation/error dialogs, and toasts. Context menus and open Spinner dropdowns close on a theme change and reopen with the new theme.

## Typography Follow-up

Only after the Pixel-Snap result passed:

- Header descriptions: 11 -> 12sp.
- Setting descriptions: 12 -> 13sp.
- Setting titles: 13 -> 14sp.
- Unit text: 11 -> 12sp.
- Restart metadata remains compact at 11sp.
- Dark muted text was raised to `#A3AFBA`; disabled text remains a separate token.

## Modified Source

- `main_kivy.py`
- `ui_theme.py`
- `ui_theme_controller.py` (new)
- `ui_geometry.py` (new)
- `ui_components.py`
- `ui_form_components.py`
- `ui_secondary_shell.py`
- `ui_settings.py`
- `ui_settings_schema.py`
- `ui_device_details.py`
- `ui_group_management.py`
- screenshot/A-B scripts and UI regression tests

## Remaining Release Work

A formal portable was not built from this uncommitted working tree. The task requires `BUILD_INFO` to identify the final pushed UI commit, so commit/push authorization and the resulting immutable commit are prerequisites. CI, source-versus-portable image comparison, portable hashes/content scans, and portable runtime checks remain `NOT_RUN` rather than being inferred from source tests.
