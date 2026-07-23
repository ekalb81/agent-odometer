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
    _tray: tauri::tray::TrayIcon<tauri::Wry>,
}

#[derive(Debug, Deserialize)]
pub struct TrayTotals {
    pub tokens: String,
    pub codex_credits: String,
    pub codex_api_usd: String,
    pub claude_usd: String,
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
        "Codex credits · —",
        false,
        None::<&str>,
    )?;
    let codex_api = MenuItem::with_id(
        app,
        "codex_api",
        "Codex API estimate · —",
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
    let show_hide =
        MenuItem::with_id(app, "show_hide", "Show / Hide Odometer", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = MenuBuilder::new(app)
        .items(&[&tokens, &codex_credits, &codex_api, &claude_usd])
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
                "settings" => {
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
    tray.tokens
        .set_text(format!("Today · {} tokens", totals.tokens))
        .map_err(|error| error.to_string())?;
    tray.codex_credits
        .set_text(format!("Codex credits · {}", totals.codex_credits))
        .map_err(|error| error.to_string())?;
    tray.codex_api
        .set_text(format!("Codex API estimate · {}", totals.codex_api_usd))
        .map_err(|error| error.to_string())?;
    tray.claude_usd
        .set_text(format!("Claude estimate · {}", totals.claude_usd))
        .map_err(|error| error.to_string())?;
    Ok(())
}
