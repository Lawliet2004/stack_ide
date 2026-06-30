//! Centralized egui texture lifecycle for image and PDF pane content.
//!
//! All GPU texture handles are owned here. Callers register textures by key
//! and evict them when panes close or content changes. This prevents GPU
//! memory leaks across file opens and closes.
//!
//! Key conventions:
//! - Image viewer:  `"image:{path}"`
//! - PDF page:      `"pdf:{path}:{page}:{zoom_int}"`

use std::collections::HashMap;

/// Central store for egui texture handles.
#[derive(Default)]
pub struct TextureRegistry {
    textures: HashMap<String, egui::TextureHandle>,
}

impl TextureRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store a texture under `key`. Replaces any existing texture.
    pub fn register(&mut self, key: impl Into<String>, texture: egui::TextureHandle) {
        self.textures.insert(key.into(), texture);
    }

    /// Retrieve a texture by key.
    pub fn get(&self, key: &str) -> Option<&egui::TextureHandle> {
        self.textures.get(key)
    }

    /// Remove and drop a texture by key (GPU memory freed immediately by egui).
    pub fn evict(&mut self, key: &str) {
        self.textures.remove(key);
    }

    /// Remove all textures whose keys start with `prefix`.
    pub fn evict_prefix(&mut self, prefix: &str) {
        self.textures.retain(|k, _| !k.starts_with(prefix));
    }

    /// Number of textures currently stored.
    pub fn len(&self) -> usize {
        self.textures.len()
    }

    pub fn is_empty(&self) -> bool {
        self.textures.is_empty()
    }
}
