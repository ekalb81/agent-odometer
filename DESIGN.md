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

## Typography

Spline Sans is the bundled interface face; Spline Sans Mono is used for metrics and model identifiers. The app base is 13px. Section labels are 10px, semibold, uppercase, and tracked by 0.07em. Larger summary values use bold 30px mono text.

## Layout

The interface fills a resizable desktop window. A 48px toolbar holds the wordmark, horizontally scrollable view tabs, organization controls, and filters; at widths below 1100px it wraps and grows vertically. The session view combines overview cards with a dense, scrollable table and optional detail pane. Table rows are 48px high; the content remains scrollable when columns exceed the available width.

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
