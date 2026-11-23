pub mod commands;
pub mod db;
pub mod env;
pub mod error;
pub mod git;
pub mod providers;
pub mod review;
pub mod secrets;
pub mod service;
pub mod settings;
pub mod tickets;
pub mod watcher;

use std::sync::Arc;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WindowEvent};
use tauri_plugin_notification::NotificationExt;

use db::models::Review;
use service::{ProgressEvent, Service, UiSink};

const TRAY_ID: &str = "main";

struct TauriSink {
    app: AppHandle,
}

impl UiSink for TauriSink {
    fn review_progress(&self, event: &ProgressEvent) {
        let _ = self.app.emit("review-progress", event);
    }

    fn review_updated(&self, review: &Review) {
        let _ = self.app.emit("review-updated", review);
    }

    fn tracked_updated(&self) {
        let _ = self.app.emit("tracked-updated", ());
        update_tray(&self.app);
    }

    fn notify(&self, title: &str, body: &str) {
        if let Err(e) = self
            .app
            .notification()
            .builder()
            .title(title)
            .body(body)
            .show()
        {
            tracing::warn!("notification failed: {e}");
        }
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

fn update_tray(app: &AppHandle) {
    let (Some(tray), Some(svc)) = (app.tray_by_id(TRAY_ID), app.try_state::<Arc<Service>>()) else {
        return;
    };
    let new_changes = svc
        .db
        .list_tracked(true)
        .map(|t| t.iter().filter(|t| t.has_new_changes).count())
        .unwrap_or(0);
    let running = svc.queue.active_count();
    let mut tooltip = String::from("PR Helper");
    if running > 0 {
        tooltip.push_str(&format!(" · {running} review(s) running"));
    }
    if new_changes > 0 {
        tooltip.push_str(&format!(" · {new_changes} PR(s) with new changes"));
    }
    let _ = tray.set_tooltip(Some(&tooltip));
    #[cfg(target_os = "macos")]
    let _ = tray.set_title(if new_changes > 0 {
        Some(new_changes.to_string())
    } else {
        None
    });
}

fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open PR Helper", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &quit])?;
    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("PR Helper")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main_window(app),
            "quit" => {
                if let Some(svc) = app.try_state::<Arc<Service>>() {
                    svc.queue.cancel_all();
                }
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    // macOS menu bar: a black/alpha template image that the system tints for light, dark and
    // highlighted states. Elsewhere the colour app icon stays visible on any taskbar.
    #[cfg(target_os = "macos")]
    {
        let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray-template.png"))?;
        builder = builder.icon(icon).icon_as_template(true);
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "pr_helper_lib=info".into()),
        )
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app)
        }))
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--hidden"]),
        ))
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let cache_dir = app.path().app_cache_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            std::fs::create_dir_all(&cache_dir)?;

            let db = Arc::new(db::Db::open(&data_dir.join("pr-helper.sqlite3"))?);
            let interrupted = db.fail_interrupted_reviews()?;
            if interrupted > 0 {
                tracing::info!("marked {interrupted} interrupted review(s) as failed");
            }
            // Leftover worktrees from interrupted runs are useless; reviews are recreated on demand.
            let keep = db.app_config().map(|c| c.keep_worktrees).unwrap_or(false);
            if !keep {
                let _ = std::fs::remove_dir_all(cache_dir.join("reviews"));
            }

            let sink = Arc::new(TauriSink {
                app: app.handle().clone(),
            });
            let svc = Service::new(db, Arc::new(secrets::KeyringStore), cache_dir, sink, None)?;
            app.manage(svc.clone());
            setup_tray(app)?;
            update_tray(app.handle());
            watcher::spawn(svc);

            if std::env::args().any(|a| a == "--hidden")
                && let Some(w) = app.get_webview_window("main")
            {
                let _ = w.hide();
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps the app (and the watcher) running in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_servers,
            commands::save_server,
            commands::delete_server,
            commands::save_server_settings,
            commands::list_ticket_sources,
            commands::save_ticket_source,
            commands::delete_ticket_source,
            commands::test_ticket_source,
            commands::list_prs,
            commands::get_pr_detail,
            commands::load_pr_tickets,
            commands::cancel_ticket_load,
            commands::load_ticket_image,
            commands::start_review,
            commands::cancel_review,
            commands::get_review,
            commands::get_review_file,
            commands::list_reviews_for_pr,
            commands::list_recent_reviews,
            commands::update_finding,
            commands::check_comment_text,
            commands::add_user_comment,
            commands::delete_user_comment,
            commands::check_user_comment,
            commands::cancel_comment_check,
            commands::post_findings,
            commands::resume_command,
            commands::list_tracked,
            commands::untrack_pr,
            commands::get_global_settings,
            commands::save_global_settings,
            commands::get_repo_settings,
            commands::save_repo_settings,
            commands::list_repo_settings,
            commands::effective_settings,
            commands::inherited_settings,
            commands::get_app_config,
            commands::save_app_config,
            commands::list_skills,
            commands::save_skill,
            commands::delete_skill,
            commands::check_environment,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            // macOS: clicking the dock icon re-opens the hidden window.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Reopen { .. } = event {
                show_main_window(app);
            }
            let _ = (app, event);
        });
}
