use crate::language::{LanguageId, LanguageServerId};
use crate::lsp::types::{
    CallHierarchyItem, CodeLensItem, LspDiagnostic, LspResponse, ProgressState, TypeHierarchyItem,
};
use crate::lsp::LspClient;
use crate::settings::{LspSettings, Settings};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerStatus {
    Running,
    Starting,
    Stopped,
}

pub struct LspManager {
    clients: HashMap<LanguageServerId, LspClient>,
    #[cfg(test)]
    client_factory: Option<Box<dyn Fn(LanguageServerId, PathBuf) -> LspClient + Send + Sync>>,
}

impl Default for LspManager {
    fn default() -> Self {
        Self::new()
    }
}

impl LspManager {
    pub fn new() -> Self {
        Self {
            clients: HashMap::new(),
            #[cfg(test)]
            client_factory: None,
        }
    }

    #[cfg(test)]
    pub fn with_factory(
        factory: Box<dyn Fn(LanguageServerId, PathBuf) -> LspClient + Send + Sync>,
    ) -> Self {
        Self {
            clients: HashMap::new(),
            client_factory: Some(factory),
        }
    }

    pub fn lazy_get_client(
        &mut self,
        server_id: LanguageServerId,
        settings: &Settings,
        root_path: &Path,
    ) -> Option<&mut LspClient> {
        if self.clients.contains_key(&server_id) {
            return self.clients.get_mut(&server_id);
        }

        if !settings.lsp.is_enabled(server_id) {
            return None;
        }

        let (command, args) = settings.lsp.server_config(server_id)?;

        let client = {
            #[cfg(test)]
            {
                if let Some(ref factory) = self.client_factory {
                    factory(server_id, root_path.to_path_buf())
                } else {
                    LspClient::start_with_config(&command, &args, root_path.to_path_buf())
                }
            }
            #[cfg(not(test))]
            {
                LspClient::start_with_config(&command, &args, root_path.to_path_buf())
            }
        };

        self.clients.insert(server_id, client);
        self.clients.get_mut(&server_id)
    }

    pub fn stop_server(&mut self, server_id: LanguageServerId) {
        if let Some(mut client) = self.clients.remove(&server_id) {
            client.shutdown_and_join();
        }
    }

    pub fn handle_settings_change(
        &mut self,
        old: &LspSettings,
        new: &LspSettings,
        _root_path: &Path,
    ) {
        if old.rust != new.rust {
            self.stop_server(LanguageServerId::Rust);
        }
        if old.python != new.python {
            self.stop_server(LanguageServerId::Python);
        }
        if old.typescript != new.typescript {
            self.stop_server(LanguageServerId::TypeScript);
        }
    }

    pub fn poll(&mut self) -> Vec<(LanguageServerId, LspResponse)> {
        let mut results = Vec::new();
        for (&server_id, client) in self.clients.iter_mut() {
            let responses = client.poll();
            for response in responses {
                results.push((server_id, response));
            }
        }
        results
    }

    pub fn did_open(
        &mut self,
        path: &Path,
        text: &str,
        version: i32,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                let lsp_lang_id = lang.lsp_language_id().unwrap_or("plain");
                return client.did_open(path, lsp_lang_id, text, version);
            }
        }
        false
    }

    pub fn did_change(
        &mut self,
        path: &Path,
        text: &str,
        version: i32,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                return client.did_change(path, text, version);
            }
        }
        false
    }

    pub fn did_close(&mut self, path: &Path) {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.clients.get_mut(&server_id) {
                client.did_close(path);
            }
        }
    }

    pub fn request_completion(
        &mut self,
        path: &Path,
        line: u32,
        col: u32,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                return client.request_completion(path, line, col, id);
            }
        }
        false
    }

    pub fn request_hover(
        &mut self,
        path: &Path,
        line: u32,
        col: u32,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                return client.request_hover(path, line, col, id);
            }
        }
        false
    }

    pub fn request_goto_definition(
        &mut self,
        path: &Path,
        line: u32,
        col: u32,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                return client.request_goto_definition(path, line, col, id);
            }
        }
        false
    }

    pub fn request_document_symbol(
        &mut self,
        path: &Path,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                if client.is_running() {
                    return client.request_document_symbol(path, id);
                }
            }
        }
        false
    }

    pub fn request_format(
        &mut self,
        path: &Path,
        tab_size: u32,
        insert_spaces: bool,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                if client.is_running() {
                    return client.request_format(path, tab_size, insert_spaces, id);
                }
            }
        }
        false
    }

    pub fn request_inlay_hints(
        &mut self,
        path: &Path,
        start_line: u32,
        end_line: u32,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                if client.is_running() {
                    return client.request_inlay_hints(path, start_line, end_line, id);
                }
            }
        }
        false
    }

    /// Request signature help. Finds the language server for `path` and enqueues
    /// [`LspRequest::SignatureHelp`]. `id` is echoed back on
    /// [`LspResponse::SignatureHelpResult`].
    pub fn request_signature_help(
        &mut self,
        path: &Path,
        line: u32,
        col: u32,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                if client.is_running() {
                    return client.request_signature_help(path, line, col, id);
                }
            }
        }
        false
    }

    /// Request workspace symbols for `query`. Sends to the server for the first running
    /// language server found. `id` is echoed back on [`LspResponse::WorkspaceSymbolResult`].
    pub fn request_workspace_symbol(
        &mut self,
        query: &str,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        // Workspace symbol searches apply to all servers; use the first running one.
        for server_id in [
            crate::language::LanguageServerId::Rust,
            crate::language::LanguageServerId::Python,
            crate::language::LanguageServerId::TypeScript,
        ] {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                if client.is_running() {
                    return client.request_workspace_symbol(query, id);
                }
            }
        }
        false
    }

    /// Request code actions at a range. Finds the server for `path` and sends
    /// [`LspRequest::CodeAction`]. `id` is echoed back on [`LspResponse::CodeActionResult`].
    pub fn request_code_action(
        &mut self,
        path: &Path,
        range: (u32, u32, u32, u32),
        diagnostics: Vec<LspDiagnostic>,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                if client.is_running() {
                    return client.request_code_action(path, range, diagnostics, id);
                }
            }
        }
        false
    }

    pub fn request_code_lens(
        &mut self,
        path: &Path,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                if client.is_running() {
                    return client.request_code_lens(path, id);
                }
            }
        }
        false
    }

    pub fn request_code_lens_resolve(
        &mut self,
        path: &Path,
        item: CodeLensItem,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                if client.is_running() {
                    return client.request_code_lens_resolve(item, id);
                }
            }
        }
        false
    }

    pub fn request_semantic_tokens_full(
        &mut self,
        path: &Path,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                if client.is_running() {
                    return client.request_semantic_tokens_full(path, id);
                }
            }
        }
        false
    }

    pub fn request_semantic_tokens_range(
        &mut self,
        path: &Path,
        start_line: u32,
        end_line: u32,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                if client.is_running() {
                    return client.request_semantic_tokens_range(path, start_line, end_line, id);
                }
            }
        }
        false
    }

    pub fn request_prepare_call_hierarchy(
        &mut self,
        path: &Path,
        line: u32,
        col: u32,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                if client.is_running() {
                    return client.request_prepare_call_hierarchy(path, line, col, id);
                }
            }
        }
        false
    }

    pub fn request_incoming_calls(
        &mut self,
        path: &Path,
        item: CallHierarchyItem,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                if client.is_running() {
                    return client.request_incoming_calls(item, id);
                }
            }
        }
        false
    }

    pub fn request_outgoing_calls(
        &mut self,
        path: &Path,
        item: CallHierarchyItem,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                if client.is_running() {
                    return client.request_outgoing_calls(item, id);
                }
            }
        }
        false
    }

    pub fn request_prepare_type_hierarchy(
        &mut self,
        path: &Path,
        line: u32,
        col: u32,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                if client.is_running() {
                    return client.request_prepare_type_hierarchy(path, line, col, id);
                }
            }
        }
        false
    }

    pub fn request_supertypes(
        &mut self,
        path: &Path,
        item: TypeHierarchyItem,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                if client.is_running() {
                    return client.request_supertypes(item, id);
                }
            }
        }
        false
    }

    pub fn request_subtypes(
        &mut self,
        path: &Path,
        item: TypeHierarchyItem,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                if client.is_running() {
                    return client.request_subtypes(item, id);
                }
            }
        }
        false
    }

    pub fn request_execute_command(
        &mut self,
        path: &Path,
        command: String,
        args: serde_json::Value,
        id: u64,
        settings: &Settings,
        root_path: &Path,
    ) -> bool {
        let lang = LanguageId::from_path(path);
        if let Some(server_id) = lang.server_id() {
            if let Some(client) = self.lazy_get_client(server_id, settings, root_path) {
                if client.is_running() {
                    return client.request_execute_command(command, args, id);
                }
            }
        }
        false
    }

    pub fn is_running(&self, server_id: LanguageServerId) -> bool {
        if let Some(client) = self.clients.get(&server_id) {
            client.is_running()
        } else {
            false
        }
    }

    pub fn is_started(&self, server_id: LanguageServerId) -> bool {
        self.clients.contains_key(&server_id)
    }

    pub fn diagnostics_for(&self, path: &Path) -> Option<&[LspDiagnostic]> {
        let lang = LanguageId::from_path(path);
        let server_id = lang.server_id()?;
        let client = self.clients.get(&server_id)?;
        client.diagnostics_for(path)
    }

    pub fn all_diagnostics(&self) -> HashMap<PathBuf, Vec<LspDiagnostic>> {
        let mut all = HashMap::new();
        for client in self.clients.values() {
            for (path, diags) in client.diagnostics() {
                all.insert(path.clone(), diags.clone());
            }
        }
        all
    }

    pub fn active_progresses(&self) -> HashMap<String, ProgressState> {
        let mut all = HashMap::new();
        for client in self.clients.values() {
            for (token, state) in &client.active_progress {
                all.insert(token.clone(), state.clone());
            }
        }
        all
    }

    pub fn shutdown_all(&mut self) {
        for mut client in self.clients.drain() {
            client.1.shutdown_and_join();
        }
    }
}

impl Drop for LspManager {
    fn drop(&mut self) {
        self.shutdown_all();
    }
}
