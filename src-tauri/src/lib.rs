mod ai;
mod index;
mod pdf;
mod providers;
mod transfer;
mod vault;
mod vaults;
mod watcher;

use ai::AppAiState;
use index::AppState;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        .manage(AppAiState::default());

    // The unlock-session state exists only when an encrypted provider is built.
    #[cfg(feature = "encryption")]
    let builder = builder.manage(providers::crypto::SecretsSession::default());

    // The tinylord login-session state exists only when that provider is built.
    #[cfg(feature = "provider-tinylord")]
    let builder = builder.manage(providers::tinylord::TinyLordSessions::default());

    builder
        .setup(|app| {
            // On startup, open the active vault's backend (plain always; an
            // encrypted vault only if it can be unlocked silently). Non-fatal:
            // the app still runs without a backend.
            let handle = app.handle().clone();
            let state = handle.state::<AppState>();
            providers::open_active_on_startup(&handle, &state);
            // Rehydrate the AI chat history from disk.
            let ai_state = handle.state::<AppAiState>();
            ai::load_history(&handle, &ai_state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            vault::pick_vault,
            vault::get_vault,
            vault::set_vault,
            vaults::list_vaults,
            vaults::add_vault,
            vaults::create_vault,
            vaults::remove_vault,
            vaults::rename_vault,
            vaults::switch_vault,
            providers::list_providers,
            providers::active_capabilities,
            #[cfg(feature = "provider-encrypted-db")]
            providers::encrypted_db::create_encrypted_vault,
            #[cfg(feature = "provider-encrypted-files")]
            providers::encrypted_files::create_encrypted_files_vault,
            #[cfg(feature = "provider-tinylord")]
            providers::tinylord::create_tinylord_vault,
            // Shared unlock/lock commands (present whenever any needs-unlock
            // provider is built), dispatching by vault kind.
            #[cfg(any(feature = "encryption", feature = "provider-tinylord"))]
            providers::unlock::vault_needs_unlock,
            #[cfg(any(feature = "encryption", feature = "provider-tinylord"))]
            providers::unlock::unlock_vault,
            #[cfg(any(feature = "encryption", feature = "provider-tinylord"))]
            providers::unlock::unlock_remembered,
            #[cfg(any(feature = "encryption", feature = "provider-tinylord"))]
            providers::unlock::lock_vault,
            vault::scan_vault,
            vault::read_note,
            vault::write_note,
            vault::create_note,
            vault::create_folder,
            vault::rename_path,
            vault::trash_path,
            vault::save_attachment,
            vault::read_attachment_data_url,
            vault::resolve_note,
            vault::resolve_or_create_note,
            vault::reveal_in_finder,
            transfer::transfer_note,
            transfer::list_vault_folders,
            #[cfg(any(feature = "encryption", feature = "provider-tinylord"))]
            transfer::unlock_transfer_dest,
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
