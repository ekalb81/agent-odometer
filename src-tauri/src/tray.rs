use crate::store::AppState;
use serde::Deserialize;
use std::sync::Arc;
use tauri::menu::{MenuBuilder, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager};

pub struct TrayState {
    pub tokens: MenuItem<tauri::Wry>,
    pub codex_credits: MenuItem<tauri::Wry>,
    pub codex_api: MenuItem<tauri::Wry>,
    pub claude_usd: MenuItem<tauri::Wry>,
    pub gemini_plan: MenuItem<tauri::Wry>,
    pub quota: MenuItem<tauri::Wry>,
    pub recent: MenuItem<tauri::Wry>,
    _tray: tauri::tray::TrayIcon<tauri::Wry>,
}

#[derive(Debug, Deserialize)]
pub struct TrayTotals {
    pub tokens: String,
    pub codex_credits: String,
    pub codex_api_usd: String,
    pub claude_usd: String,
    #[serde(default)]
    pub gemini_plan: String,
    /// Live-quota headroom label (issue #43), e.g. "codex 5h 37% left".
    /// Empty when nothing is available — the menu item then falls back to
    /// its placeholder text rather than rendering blank. Formatted
    /// frontend-side by `quotaTrayLabel` from numbers `crate::quota`
    /// already computed; this module only displays the string.
    #[serde(default)]
    pub quota: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub recent: String,
}

pub fn start(app: &tauri::AppHandle, state: &Arc<AppState>) -> tauri::Result<()> {
    let tokens = MenuItem::with_id(
        app,
        "today_tokens",
        "Today · loading usage…",
        false,
        None::<&str>,
    )?;
    let codex_credits = MenuItem::with_id(
        app,
        "codex_credits",
        "Codex purchased-credit estimate · —",
        false,
        None::<&str>,
    )?;
    let codex_api = MenuItem::with_id(
        app,
        "codex_api",
        "Codex API base estimate · —",
        false,
        None::<&str>,
    )?;
    let claude_usd = MenuItem::with_id(
        app,
        "claude_usd",
        "Claude estimate · —",
        false,
        None::<&str>,
    )?;
    let gemini_plan = MenuItem::with_id(
        app,
        "gemini_plan",
        "Gemini plan estimate · —",
        false,
        None::<&str>,
    )?;
    let quota = MenuItem::with_id(app, "quota", "Quota · —", false, None::<&str>)?;
    let recent = MenuItem::with_id(
        app,
        "recent_alerts",
        "Recent alerts · none",
        true,
        None::<&str>,
    )?;
    let all = MenuItem::with_id(
        app,
        "provider_all",
        "Show all providers",
        true,
        None::<&str>,
    )?;
    let codex = MenuItem::with_id(app, "provider_codex", "Show Codex", true, None::<&str>)?;
    let claude = MenuItem::with_id(
        app,
        "provider_claude_code",
        "Show Claude Code",
        true,
        None::<&str>,
    )?;
    let gemini = MenuItem::with_id(
        app,
        "provider_gemini_cli",
        "Show Gemini CLI",
        true,
        None::<&str>,
    )?;
    let show_hide =
        MenuItem::with_id(app, "show_hide", "Show / Hide Odometer", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = MenuBuilder::new(app)
        .items(&[
            &tokens,
            &codex_credits,
            &codex_api,
            &claude_usd,
            &gemini_plan,
            &quota,
            &recent,
        ])
        .separator()
        .items(&[&all, &codex, &claude, &gemini])
        .separator()
        .items(&[&show_hide, &settings, &quit])
        .build()?;
    let mut builder = TrayIconBuilder::with_id("odometer")
        .menu(&menu)
        .tooltip("Odometer");
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let tray = builder
        .on_menu_event(|app, event| {
            if event.id().as_ref() == "quit" {
                app.exit(0);
                return;
            }
            let Some(window) = app.get_webview_window("main") else {
                return;
            };
            match event.id().as_ref() {
                "show_hide" => {
                    if window.is_visible().unwrap_or(false) {
                        let _ = window.hide();
                    } else {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
                "provider_all"
                | "provider_codex"
                | "provider_claude_code"
                | "provider_gemini_cli" => {
                    let provider = event
                        .id()
                        .as_ref()
                        .strip_prefix("provider_")
                        .unwrap_or("all");
                    let _ = app.emit("tray-provider-selected", provider);
                }
                "settings" | "recent_alerts" => {
                    let _ = window.show();
                    let _ = window.set_focus();
                    let _ = app.emit("open-settings", ());
                }
                _ => {}
            }
        })
        .build(app)?;
    *state.tray.lock().unwrap() = Some(TrayState {
        tokens,
        codex_credits,
        codex_api,
        claude_usd,
        gemini_plan,
        quota,
        recent,
        _tray: tray,
    });
    state
        .tray_available
        .store(true, std::sync::atomic::Ordering::Release);
    Ok(())
}

pub fn update(state: &Arc<AppState>, totals: TrayTotals) -> Result<(), String> {
    let guard = state.tray.lock().unwrap();
    let Some(tray) = guard.as_ref() else {
        return Ok(());
    };
    let scope = match totals.provider.as_str() {
        "codex" => "Codex",
        "claude_code" => "Claude Code",
        "gemini_cli" => "Gemini CLI",
        _ => "All providers",
    };
    tray.recent
        .set_text(format!(
            "Recent alerts · {}",
            if totals.recent.is_empty() {
                "none"
            } else {
                &totals.recent
            }
        ))
        .map_err(|error| error.to_string())?;
    tray.tokens
        .set_text(format!("Today · {scope} · {} tokens", totals.tokens))
        .map_err(|error| error.to_string())?;
    let codex_selected = matches!(totals.provider.as_str(), "codex" | "all" | "");
    let claude_selected = matches!(totals.provider.as_str(), "claude_code" | "all" | "");
    let gemini_selected = matches!(totals.provider.as_str(), "gemini_cli" | "all" | "");
    tray.gemini_plan
        .set_text(format!(
            "Gemini plan estimate · {}",
            if gemini_selected {
                if totals.gemini_plan.is_empty() {
                    "unavailable"
                } else {
                    &totals.gemini_plan
                }
            } else {
                "not selected"
            }
        ))
        .map_err(|error| error.to_string())?;
    tray.codex_credits
        .set_text(format!(
            "Codex purchased-credit estimate · {}",
            if codex_selected {
                &totals.codex_credits
            } else {
                "not selected"
            }
        ))
        .map_err(|error| error.to_string())?;
    tray.codex_api
        .set_text(format!(
            "Codex API base estimate · {}",
            if codex_selected {
                &totals.codex_api_usd
            } else {
                "not selected"
            }
        ))
        .map_err(|error| error.to_string())?;
    tray.claude_usd
        .set_text(format!(
            "Claude estimate · {}",
            if claude_selected {
                &totals.claude_usd
            } else {
                "not selected"
            }
        ))
        .map_err(|error| error.to_string())?;
    let quota_text = if totals.quota.is_empty() {
        "Quota · —".to_string()
    } else {
        format!("Quota · {}", totals.quota)
    };
    tray.quota
        .set_text(quota_text)
        .map_err(|error| error.to_string())?;
    Ok(())
}
