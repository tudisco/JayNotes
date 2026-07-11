mod ai;
mod index;
mod pdf;
mod vault;
mod watcher;

use std::path::Path;

use ai::AppAiState;
use index::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .manage(AppAiState::default())
        .setup(|app| {
            // On startup, open + scan the index for the saved vault (if any).
            // Failures here are non-fatal: the app still runs without search.
            let handle = app.handle().clone();
            if let Some(root) = vault::saved_vault_root(&handle) {
                let state = handle.state::<AppState>();
                if let Err(e) = index::init_for_vault(&handle, &state, Path::new(&root)) {
                    eprintln!("Index init failed: {e}");
                }
            }
            // Rehydrate the AI chat history from disk.
            let ai_state = handle.state::<AppAiState>();
            ai::load_history(&handle, &ai_state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            vault::pick_vault,
            vault::get_vault,
            vault::set_vault,
            vault::scan_vault,
            vault::read_note,
            vault::write_note,
            vault::create_note,
            vault::create_folder,
            vault::rename_path,
            vault::trash_path,
            vault::save_attachment,
            vault::resolve_note,
            vault::resolve_or_create_note,
            vault::reveal_in_finder,
            pdf::export_note_pdf,
            index::reindex_vault,
            index::index_status,
            index::search_notes,
            index::list_notes,
            index::list_tags,
            index::notes_by_tag,
            ai::settings::get_ai_settings,
            ai::settings::set_ai_settings,
            ai::settings::list_ai_models,
            ai::ai_chat_send,
            ai::ai_cancel,
            ai::ai_new_chat,
            ai::ai_get_history,
            ai::ai_permission_respond,
            ai::ai_list_revisions,
            ai::ai_revert,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
