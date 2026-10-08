---
name: Odometer
description: Local agent-usage ledger for Codex, Claude Code, and Gemini CLI.
colors:
  app-dark: "#17191e"
  chrome-dark: "#1d2026"
  card-dark: "#1e2127"
  app-light: "#f7f6f3"
  chrome-light: "#ffffff"
  text-dark: "#e8eaee"
  text-light: "#1b1d22"
  codex-accent: "#4c8dff"
  codex-tab: "#2b58c9"
  claude-accent: "#e8935a"
  positive-dark: "#3ecf8e"
  positive-light: "#2f9e6a"
  warning-light: "#9a4d00"
typography:
  body:
    fontFamily: "Spline Sans, system-ui, -apple-system, Segoe UI, sans-serif"
    fontSize: "13px"
    fontWeight: 400
  metrics:
    fontFamily: "Spline Sans Mono, ui-monospace, SFMono-Regular, Menlo, monospace"
    fontWeight: 400
  section-label:
    fontFamily: "Spline Sans, system-ui, -apple-system, Segoe UI, sans-serif"
    fontSize: "10px"
    fontWeight: 600
    letterSpacing: "0.07em"
  tab-label:
    fontFamily: "Spline Sans, system-ui, -apple-system, Segoe UI, sans-serif"
    fontSize: "12px"
    fontWeight: 600
rounded:
  tab: "6px"
  control: "8px"
  card: "12px"
spacing:
  compact: "4px"
  control: "8px"
  section: "16px"
components:
  view-tab-codex-active:
    backgroundColor: "{colors.codex-tab}"
    textColor: "#ffffff"
    typography: "{typography.tab-label}"
    rounded: "{rounded.tab}"
    padding: "5px 16px"
  view-tab-claude-active:
    backgroundColor: "{colors.claude-accent}"
    textColor: "#ffffff"
    typography: "{typography.tab-label}"
    rounded: "{rounded.tab}"
    padding: "5px 16px"
  view-tab-all-active:
    backgroundColor: "var(--text, #e8eaee)"
    textColor: "var(--bg, #17191e)"
    typography: "{typography.tab-label}"
    rounded: "{rounded.tab}"
    padding: "5px 16px"
  search-input:
    backgroundColor: "{colors.app-dark}"
    textColor: "{colors.text-dark}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    height: "30px"
---

## Overview

The stylesheet names the existing system “Instrument Ledger.” Odometer is a compact desktop interface for inspecting local agent usage. Its toolbar switches between combined and provider views; overview cards and a dense session table lead into session details. Values use a mono font where numeric scanning matters.

## Colors

Dark and light themes change the app, chrome, card, text, border, and positive-status colors through `data-theme` on the document root. Provider views carry their own accent: blue for Codex and orange for Claude Code. Accents also color selected tabs, cost figures, chips, charts, and selected rows; light mode uses darker cost and chip text for legibility. Gemini CLI is supported, but its accent assignment is not described here because the inspected shared stylesheet does not define one.

Warning text uses `#9a4d00` in light mode so unavailable measurements, fallback pricing, and recovery notices remain readable on white and warm-white surfaces. Dark mode retains its existing amber. Important qualifications use at least the 12px interface text size; repeated methodology belongs in native disclosures.

## Typography

Spline Sans is the bundled interface face; Spline Sans Mono is used for metrics and model identifiers. The app base is 13px. Section labels are 10px, semibold, uppercase, and tracked by 0.07em. Larger summary values use bold 30px mono text.

## Layout

The interface fills a resizable desktop window. A 48px toolbar holds the wordmark, horizontally scrollable provider tabs, organization controls, and filters; at widths below 1100px it wraps and grows vertically. Sessions and Analytics are separate workspaces within the same provider/filter scope. Analytics keeps a compact scope strip above one scrolling report body; its four groups are Usage, Tools & context, Changes & review, and Outcomes. The session table keeps 48px rows and at least 480px of list space beside a resizable detail pane: 560px preferred, 410px minimum, 800px maximum. Below 1100px, details use a native dialog instead of squeezing the table. Settings uses a 180px six-section index, replaced below 800px by a native selector. Visited sections stay mounted to preserve drafts, while hidden reports pause their observation timers.

## Elevation & Depth

Most surfaces are separated by tonal backgrounds and thin borders. Filter popovers use a raised card surface with a shadow; the main panels and cards do not depend on prominent shadows.

## Shapes

Tabs use gently rounded corners (6px), inputs and toolbar controls use 8px corners, and overview cards and filter popovers use 12px corners. Status and provider chips are pill-shaped.

## Components

The active Codex and Claude Code tabs use their respective blue and orange fills with white semibold text. The All tab uses an inverted neutral fill and app-colored text. Inactive tabs use muted text and brighten on hover. Search and date controls use the app surface, a thin border, and an accent focus ring. Filter popovers group compact controls on a card surface. Overview cards use a thin border and rounded corners; the session table uses compact uppercase column labels, subtle row dividers, and a tinted selected row.

## Do's and Don'ts

- Do keep the compact 13px interface scale and mono styling for numeric values.
- Do preserve the distinct Codex blue and Claude Code orange accents across provider views.
- Do retain the theme-dependent foreground and surface colors when adding controls.
- Don't use the provider active fill for the All tab; it uses the inverted neutral treatment.
- Do keep scope visible across Sessions and Analytics and preserve drafts across Settings navigation.
- Do disclose pricing evidence by model without hiding unavailable, stale, or unverified pricing warnings.
