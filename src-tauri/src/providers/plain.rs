//! The built-in **plain** provider: a folder of `.md` files on disk.
//!
//! This is the reference [`VaultHandle`]. Every method forwards to the existing
//! file-operation cores in [`crate::vault`], so the plain path is byte-for-byte
//! the pre-M14 behaviour — the whole point of routing it through the handle is
//! that the *command* layer no longer branches on kind. Search stays on the
//! separate `state.index` (this handle reports `owns_index() == false`).

use std::path::{Path, PathBuf};

use crate::index::AppState;
use crate::providers::{field, Capabilities, ProviderMeta, VaultHandle, VaultProvider};
use crate::vault::{self, TreeNode};

/// Registry entry for the plain kind.
pub struct PlainProvider;

impl VaultProvider for PlainProvider {
    fn kind(&self) -> &'static str {
        "plain"
    }

    fn metadata(&self) -> ProviderMeta {
        ProviderMeta {
            kind: "plain".into(),
            display_name: "Plain folder".into(),
            description: "A folder of portable Markdown files on disk.".into(),
            config_fields: vec![
                field(
                    "location",
                    "Parent folder",
                    "folder",
                    true,
                    "Where to create the vault folder",
                ),
                field("name", "Vault name", "text", true, "My Notes"),
            ],
            capabilities: Self::CAPS,
        }
    }
}

impl PlainProvider {
    pub const CAPS: Capabilities = Capabilities {
        reveal_in_finder: true,
        needs_unlock: false,
        folder_backed: true,
    };
}

/// The opened handle for a plain vault: just its canonicalized root.
pub struct PlainHandle {
    root: PathBuf,
}

impl PlainHandle {
    pub fn new(root: &Path) -> Self {
        PlainHandle {
            root: root.to_path_buf(),
        }
    }
}

impl VaultHandle for PlainHandle {
    fn capabilities(&self) -> Capabilities {
        PlainProvider::CAPS
    }

    fn scan_tree(&self) -> Result<TreeNode, String> {
        vault::scan_tree(&self.root)
    }

    fn read_note(&self, rel: &str) -> Result<String, String> {
        vault::read_note_core(&self.root, rel)
    }

    fn write_note(&self, state: &AppState, rel: &str, content: &str) -> Result<(), String> {
        vault::write_note_at(&self.root, state, rel, content)
    }

    fn create_note(&self, state: &AppState, rel: &str) -> Result<String, String> {
        vault::create_note_core(&self.root, state, rel)
    }

    fn create_folder(&self, rel: &str) -> Result<(), String> {
        vault::create_folder_at(&self.root, rel)
    }

    fn rename(&self, state: &AppState, old_rel: &str, new_rel: &str) -> Result<(), String> {
        vault::rename_at(&self.root, state, old_rel, new_rel)
    }

    fn trash(&self, state: &AppState, rel: &str) -> Result<(), String> {
        vault::trash_at(&self.root, state, rel)
    }

    fn save_attachment(
        &self,
        state: &AppState,
        file_name: &str,
        data: &[u8],
    ) -> Result<String, String> {
        vault::save_attachment_core(&self.root, state, file_name, data)
    }

    fn read_attachment(&self, rel: &str) -> Result<Vec<u8>, String> {
        vault::read_attachment_core(&self.root, rel)
    }

    fn reveal_in_finder(&self, rel: &str) -> Result<(), String> {
        vault::reveal_core(&self.root, rel)
    }
}
