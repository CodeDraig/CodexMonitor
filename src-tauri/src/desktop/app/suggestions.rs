//! Key-scoped composer suggestions for the native desktop.
//!
//! The widget only enqueues reads through the backend client. Replies return via
//! `SuggestionState::apply_result`, keyed by the request UUID returned by the
//! controller. ACP conversations use only commands they have advertised in
//! their own conversation history and never request Codex-only catalogs.

use super::*;
use crate::client::contracts::{
    BackendError, BackendRequest, CodexRequest, ConversationKey, ConversationRef, FilesRequest,
    PromptsRequest,
};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum SuggestionSource {
    Prompts,
    Skills,
    Files,
}

#[derive(Clone, Debug)]
struct PendingSuggestionRequest {
    key: ConversationKey,
    source: SuggestionSource,
}

#[derive(Clone, Debug, Default)]
struct SuggestionCatalog {
    prompts: Vec<Value>,
    skills: Vec<Value>,
    files: Vec<String>,
    loaded: HashSet<SuggestionSource>,
    errors: HashMap<SuggestionSource, String>,
}

#[derive(Clone, Debug)]
struct Candidate {
    id: String,
    label: String,
    detail: String,
    insertion: String,
    cursor_offset: Option<usize>,
}

#[derive(Clone, Debug)]
struct ActiveTrigger {
    character: char,
    start: usize,
    end: usize,
    query: String,
}

/// Cached data and request correlation for the composer suggestion surface.
#[derive(Default)]
pub(crate) struct SuggestionState {
    catalogs: HashMap<ConversationKey, SuggestionCatalog>,
    pending: HashMap<Uuid, PendingSuggestionRequest>,
    selected_index: usize,
    signature: Option<(ConversationKey, char, String)>,
    dismissed: bool,
}

impl SuggestionState {
    /// Expand a Codex custom-prompt command using only the catalog already
    /// loaded for this conversation. ACP slash commands remain native text.
    pub(crate) fn expand_prompt(
        &self,
        key: &ConversationKey,
        text: &str,
    ) -> Result<String, String> {
        if !matches!(&key.conversation, ConversationRef::Codex { .. }) {
            return Ok(text.to_owned());
        }
        let Some((prompt_name, args)) = parse_prompt_command(text) else {
            return Ok(text.to_owned());
        };
        if prompt_name.is_empty() {
            return Ok(text.to_owned());
        }

        let Some(catalog) = self.catalogs.get(key) else {
            return Err(
                "Custom prompt data is unavailable. Custom prompts are still loading. Wait for the prompt list to load, then retry."
                    .into(),
            );
        };
        if !catalog.loaded.contains(&SuggestionSource::Prompts) {
            let detail = catalog
                .errors
                .get(&SuggestionSource::Prompts)
                .map(|error| format!(" Custom prompt discovery failed: {error}"))
                .unwrap_or_else(|| " Custom prompts are still loading.".to_owned());
            return Err(format!(
                "Custom prompt data is unavailable.{detail} Wait for the prompt list to load, then retry."
            ));
        }
        let Some(prompt) = catalog
            .prompts
            .iter()
            .find(|prompt| prompt["name"].as_str() == Some(prompt_name.as_str()))
        else {
            // The existing composer treats unrecognized prompt commands as
            // ordinary text and leaves the agent/runtime to interpret them.
            return Ok(text.to_owned());
        };

        let content = prompt["content"].as_str().unwrap_or_default();
        let required = prompt_argument_names(content);
        if !required.is_empty() {
            let inputs = parse_prompt_inputs(&args).map_err(|error| {
                match error {
                    PromptArgsError::MissingAssignment(token) => format!(
                        "Could not parse /prompts:{prompt_name}: expected key=value but found '{token}'. Wrap values in double quotes if they contain spaces."
                    ),
                    PromptArgsError::MissingKey(token) => format!(
                        "Could not parse /prompts:{prompt_name}: expected a name before '=' in '{token}'."
                    ),
                }
            })?;
            let missing: Vec<_> = required
                .iter()
                .filter(|name| !inputs.contains_key(name.as_str()))
                .cloned()
                .collect();
            if !missing.is_empty() {
                return Err(format!(
                    "Missing required args for /prompts:{prompt_name}: {}. Provide as key=value (quote values with spaces).",
                    missing.join(", ")
                ));
            }
            Ok(expand_named_placeholders(content, &inputs))
        } else {
            let positional = split_shlex(&normalize_quotes(&args));
            Ok(expand_numeric_placeholders(content, &positional))
        }
    }

    /// Apply a reply from `Controller::take_misc_results`. Returns true when the
    /// request UUID belongs to this state, including failed requests.
    pub(crate) fn apply_result(
        &mut self,
        request_id: Uuid,
        result: Result<Value, BackendError>,
    ) -> bool {
        let Some(pending) = self.pending.remove(&request_id) else {
            return false;
        };
        let catalog = self.catalogs.entry(pending.key).or_default();
        match result {
            Ok(value) => {
                catalog.errors.remove(&pending.source);
                catalog.loaded.insert(pending.source);
                match pending.source {
                    SuggestionSource::Prompts => {
                        catalog.prompts = value.as_array().cloned().unwrap_or_default();
                    }
                    SuggestionSource::Skills => {
                        catalog.skills = value.as_array().cloned().unwrap_or_default();
                    }
                    SuggestionSource::Files => {
                        catalog.files = value
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect();
                    }
                }
            }
            Err(error) => {
                catalog.errors.insert(pending.source, error.message);
            }
        }
        true
    }

    fn is_pending(&self, key: &ConversationKey, source: SuggestionSource) -> bool {
        self.pending
            .values()
            .any(|pending| &pending.key == key && pending.source == source)
    }

    fn should_request(&self, key: &ConversationKey, source: SuggestionSource) -> bool {
        !self.is_pending(key, source)
            && self.catalogs.get(key).is_none_or(|catalog| {
                !catalog.loaded.contains(&source) && !catalog.errors.contains_key(&source)
            })
    }

    fn track(&mut self, request_id: Uuid, key: ConversationKey, source: SuggestionSource) {
        self.pending
            .insert(request_id, PendingSuggestionRequest { key, source });
    }

    fn clear_errors(&mut self, key: &ConversationKey) {
        if let Some(catalog) = self.catalogs.get_mut(key) {
            catalog.errors.clear();
        }
    }
}

impl NativeApp {
    /// Draw the active autocomplete list and return a replacement draft when an
    /// item is selected. Call before drawing the composer TextEdit so key events
    /// used for navigation are consumed before the editor sees them.
    pub(crate) fn show_suggestions(
        &mut self,
        ui: &mut egui::Ui,
        key: &ConversationKey,
        draft: &str,
    ) -> Option<String> {
        let editor_id = egui::Id::new(("composer", key));
        let cursor = egui::widgets::text_edit::TextEditState::load(ui.ctx(), editor_id)
            .and_then(|state| state.cursor.char_range())
            .filter(|range| range.primary == range.secondary)
            .map(|range| range.primary.index.0)
            .unwrap_or_else(|| draft.chars().count());
        let cursor_byte = char_to_byte(draft, cursor);
        let Some(trigger) = active_trigger(draft, cursor_byte) else {
            self.suggestions.signature = None;
            self.suggestions.dismissed = false;
            self.suggestions.selected_index = 0;
            return None;
        };
        let signature = (key.clone(), trigger.character, trigger.query.clone());
        if self.suggestions.signature.as_ref() != Some(&signature) {
            self.suggestions.signature = Some(signature);
            self.suggestions.selected_index = 0;
            self.suggestions.dismissed = false;
        }
        if self.suggestions.dismissed {
            return None;
        }

        let is_codex = matches!(&key.conversation, ConversationRef::Codex { .. });
        let mut wanted = Vec::new();
        if is_codex {
            match trigger.character {
                '/' => wanted.push(SuggestionSource::Prompts),
                '$' => wanted.push(SuggestionSource::Skills),
                '@' => wanted.push(SuggestionSource::Files),
                _ => {}
            }
        }
        for source in wanted {
            if self.suggestions.should_request(key, source) {
                let (request, rpc) = match (&key.conversation, source) {
                    (ConversationRef::Codex { workspace_id, .. }, SuggestionSource::Prompts) => (
                        BackendRequest::Prompts(PromptsRequest::read(
                            "prompts_list",
                            json!({"workspaceId":workspace_id}),
                        )),
                        source,
                    ),
                    (ConversationRef::Codex { workspace_id, .. }, SuggestionSource::Skills) => (
                        BackendRequest::Codex(CodexRequest::read(
                            "skills_list",
                            json!({"workspaceId":workspace_id}),
                        )),
                        source,
                    ),
                    (ConversationRef::Codex { workspace_id, .. }, SuggestionSource::Files) => (
                        BackendRequest::Files(FilesRequest::read(
                            "list_workspace_files",
                            json!({"workspaceId":workspace_id}),
                        )),
                        source,
                    ),
                    (ConversationRef::Acp { .. }, _) => continue,
                };
                match self.controller.request(request) {
                    Ok(request_id) => self.suggestions.track(request_id, key.clone(), rpc),
                    Err(error) => self
                        .notices
                        .push(format!("Could not load composer suggestions: {error:?}")),
                }
            }
        }

        let mut candidates = match trigger.character {
            '/' if is_codex => codex_slash_candidates(self.suggestions.catalogs.get(key)),
            '/' => {
                let items = self
                    .controller
                    .state
                    .selected_conversation()
                    .map(|conversation| conversation.items.as_slice())
                    .unwrap_or_default();
                runtime_slash_candidates(items)
            }
            '$' if is_codex => skill_candidates(self.suggestions.catalogs.get(key)),
            '@' if is_codex => file_candidates(self.suggestions.catalogs.get(key)),
            _ => Vec::new(),
        };
        candidates = rank_candidates(candidates, &trigger.query);
        let catalog = self.suggestions.catalogs.get(key);
        let error = catalog
            .and_then(|catalog| match trigger.character {
                '/' => catalog.errors.get(&SuggestionSource::Prompts),
                '$' => catalog.errors.get(&SuggestionSource::Skills),
                '@' => catalog.errors.get(&SuggestionSource::Files),
                _ => None,
            })
            .cloned();
        let has_error = error.is_some();
        if let Some(error) = error {
            ui.horizontal(|ui| {
                ui.colored_label(theme::ERROR, format!("Suggestion source failed: {error}"));
                if ui.small_button("Retry").clicked() {
                    self.suggestions.clear_errors(key);
                }
            });
        }
        if candidates.is_empty() {
            let pending = match trigger.character {
                '/' => self.suggestions.is_pending(key, SuggestionSource::Prompts),
                '$' => self.suggestions.is_pending(key, SuggestionSource::Skills),
                '@' => self.suggestions.is_pending(key, SuggestionSource::Files),
                _ => false,
            };
            if pending || has_error {
                ui.group(|ui| {
                    ui.label(match trigger.character {
                        '/' => "Slash suggestions",
                        '$' => "Skills",
                        _ => "Workspace files",
                    });
                    if !has_error {
                        ui.spinner();
                        ui.label("Loading suggestions…");
                    }
                });
            }
            return None;
        }

        self.suggestions.selected_index %= candidates.len();
        let move_up = consume_suggestion_key(ui.ctx(), editor_id, egui::Key::ArrowUp);
        let move_down = consume_suggestion_key(ui.ctx(), editor_id, egui::Key::ArrowDown);
        if move_up {
            self.suggestions.selected_index =
                (self.suggestions.selected_index + candidates.len() - 1) % candidates.len();
        }
        if move_down {
            self.suggestions.selected_index =
                (self.suggestions.selected_index + 1) % candidates.len();
        }
        let escape = consume_suggestion_key(ui.ctx(), editor_id, egui::Key::Escape);
        if escape {
            self.suggestions.dismissed = true;
            return None;
        }
        let enter = consume_suggestion_key(ui.ctx(), editor_id, egui::Key::Enter);
        let mut chosen = enter.then(|| candidates[self.suggestions.selected_index].clone());
        ui.group(|ui| {
            ui.label(match trigger.character {
                '/' => "Commands and prompts",
                '$' => "Skills",
                _ => "Workspace files",
            });
            egui::ScrollArea::vertical()
                .id_salt(("composer-suggestions", key))
                .max_height(180.0)
                .show(ui, |ui| {
                    for (index, candidate) in candidates.iter().enumerate() {
                        ui.push_id(&candidate.id, |ui| {
                            let response = ui.selectable_label(
                                self.suggestions.selected_index == index,
                                &candidate.label,
                            );
                            if !candidate.detail.is_empty() {
                                response.clone().on_hover_text(&candidate.detail);
                            }
                            if response.clicked() {
                                chosen = Some(candidate.clone());
                            }
                        });
                    }
                });
        });
        let Some(candidate) = chosen else {
            return None;
        };
        let trigger_bytes = trigger.character.len_utf8();
        let before_end = trigger.start + trigger_bytes;
        let before = &draft[..before_end];
        let after = &draft[trigger.end..];
        let needs_space =
            !after.is_empty() && !after.chars().next().is_some_and(char::is_whitespace);
        let suffix = if candidate.cursor_offset.is_some() {
            ""
        } else if after.is_empty() || needs_space {
            " "
        } else {
            ""
        };
        let replacement = format!("{before}{}{suffix}{after}", candidate.insertion);
        let cursor_char = before.chars().count()
            + candidate
                .cursor_offset
                .unwrap_or_else(|| candidate.insertion.chars().count())
            + suffix.chars().count();
        if let Some(mut editor) = egui::widgets::text_edit::TextEditState::load(ui.ctx(), editor_id)
        {
            editor
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(cursor_char),
                )));
            editor.store(ui.ctx(), editor_id);
        }
        self.suggestions.signature = None;
        self.suggestions.dismissed = false;
        Some(replacement)
    }
}

fn char_to_byte(text: &str, char_offset: usize) -> usize {
    text.char_indices()
        .nth(char_offset)
        .map(|(byte, _)| byte)
        .unwrap_or(text.len())
}

fn consume_suggestion_key(ctx: &egui::Context, editor_id: egui::Id, key: egui::Key) -> bool {
    if !ctx.memory(|memory| memory.has_focus(editor_id)) {
        return false;
    }
    ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, key))
}

fn active_trigger(text: &str, cursor_byte: usize) -> Option<ActiveTrigger> {
    let prefix = text.get(..cursor_byte)?;
    let (start, character) = prefix.char_indices().rev().find(|(start, character)| {
        if !matches!(*character, '/' | '$' | '@') {
            return false;
        }
        let previous = prefix[..*start].chars().next_back();
        previous.is_none_or(|previous| {
            previous.is_whitespace() || matches!(previous, '(' | '[' | '{' | '"' | '\'' | '`')
        })
    })?;
    let query_start = start + character.len_utf8();
    let query = prefix.get(query_start..)?;
    if query.chars().any(char::is_whitespace) {
        return None;
    }
    Some(ActiveTrigger {
        character,
        start,
        end: cursor_byte,
        query: query.to_owned(),
    })
}

fn rank_candidates(mut candidates: Vec<Candidate>, query: &str) -> Vec<Candidate> {
    let query = query.to_lowercase();
    candidates
        .retain(|candidate| query.is_empty() || candidate.label.to_lowercase().contains(&query));
    candidates.sort_by(|a, b| {
        let score = |candidate: &Candidate| {
            let label = candidate.label.to_lowercase();
            if query.is_empty() || label.starts_with(&query) {
                0
            } else {
                1
            }
        };
        score(a).cmp(&score(b)).then_with(|| a.label.cmp(&b.label))
    });
    candidates.truncate(40);
    candidates
}

fn codex_slash_candidates(catalog: Option<&SuggestionCatalog>) -> Vec<Candidate> {
    let mut candidates: Vec<_> = [
        ("apps", "list available apps"),
        ("compact", "compact the active thread context"),
        ("fast", "toggle Fast mode for upcoming turns"),
        ("fork", "branch into a new thread"),
        ("mcp", "list configured MCP tools"),
        ("new", "start a new chat"),
        ("resume", "refresh the active thread"),
        ("review", "start a code review"),
        ("status", "show session status"),
    ]
    .into_iter()
    .map(|(name, detail)| Candidate {
        id: format!("command:{name}"),
        label: name.into(),
        detail: detail.into(),
        insertion: name.into(),
        cursor_offset: None,
    })
    .collect();
    if let Some(catalog) = catalog {
        candidates.extend(catalog.prompts.iter().filter_map(|prompt| {
            let name = prompt["name"].as_str()?;
            let content = prompt["content"].as_str().unwrap_or_default();
            let (full_insert, cursor_offset) = prompt_insert_parts(name, content, false);
            let insertion = full_insert
                .strip_prefix('/')
                .unwrap_or(&full_insert)
                .to_owned();
            Some(Candidate {
                id: format!("prompt:{name}"),
                label: format!("prompts:{name}"),
                detail: prompt["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                insertion,
                cursor_offset,
            })
        }));
    }
    candidates
}

fn prompt_argument_names(content: &str) -> Vec<String> {
    let chars: Vec<char> = content.chars().collect();
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '$' || (index > 0 && chars[index - 1] == '$') {
            index += 1;
            continue;
        }
        let start = index + 1;
        if !chars.get(start).is_some_and(char::is_ascii_uppercase) {
            index += 1;
            continue;
        }
        let mut end = start + 1;
        while chars.get(end).is_some_and(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || *character == '_'
        }) {
            end += 1;
        }
        let name: String = chars[start..end].iter().collect();
        if name != "ARGUMENTS" && seen.insert(name.clone()) {
            names.push(name);
        }
        index = end;
    }
    names
}

/// Build the prompt command inserted by the Workbench prompt panel.
pub(super) fn prompt_insert_text(prompt: &Value) -> String {
    let name = prompt["name"].as_str().unwrap_or_default();
    let content = prompt["content"].as_str().unwrap_or_default();
    prompt_insert_parts(name, content, true).0
}

fn prompt_insert_parts(name: &str, content: &str, include_slash: bool) -> (String, Option<usize>) {
    let mut insertion = if include_slash {
        format!("/prompts:{name}")
    } else {
        format!("prompts:{name}")
    };
    let mut cursor_offset = None;
    for arg in prompt_argument_names(content) {
        if cursor_offset.is_none() {
            cursor_offset = Some(insertion.chars().count() + 1 + arg.chars().count() + 2);
        }
        insertion.push_str(&format!(" {arg}=\"\""));
    }
    (insertion, cursor_offset)
}

fn parse_prompt_command(text: &str) -> Option<(String, String)> {
    let stripped = text.strip_prefix('/')?;
    let name_end = stripped
        .char_indices()
        .find(|(_, ch)| ch.is_whitespace())
        .map(|(index, _)| index)
        .unwrap_or(stripped.len());
    let name = &stripped[..name_end];
    if !name.starts_with("prompts:") {
        return None;
    }
    Some((
        name["prompts:".len()..].to_owned(),
        stripped[name_end..].trim_start().to_owned(),
    ))
}

#[derive(Debug, PartialEq, Eq)]
enum PromptArgsError {
    MissingAssignment(String),
    MissingKey(String),
}

fn normalize_quotes(input: &str) -> String {
    input
        .replace('\u{201C}', "\"")
        .replace('\u{201D}', "\"")
        .replace('\u{2018}', "'")
        .replace('\u{2019}', "'")
}

fn split_shlex(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if !in_single && ch == '\\' {
            escaped = true;
            continue;
        }
        if !in_double && ch == '\'' {
            in_single = !in_single;
            continue;
        }
        if !in_single && ch == '"' {
            in_double = !in_double;
            continue;
        }
        if !in_single && !in_double && ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(ch);
    }
    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

fn parse_prompt_inputs(rest: &str) -> Result<HashMap<String, String>, PromptArgsError> {
    let mut values = HashMap::new();
    if rest.trim().is_empty() {
        return Ok(values);
    }
    for token in split_shlex(&normalize_quotes(rest)) {
        let Some(eq_index) = token.find('=') else {
            return Err(PromptArgsError::MissingAssignment(token));
        };
        if eq_index == 0 {
            return Err(PromptArgsError::MissingKey(token));
        }
        values.insert(
            token[..eq_index].to_owned(),
            token[eq_index + 1..].to_owned(),
        );
    }
    Ok(values)
}

fn expand_named_placeholders(content: &str, inputs: &HashMap<String, String>) -> String {
    let chars: Vec<char> = content.chars().collect();
    let mut output = String::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '$' || (index > 0 && chars[index - 1] == '$') {
            output.push(chars[index]);
            index += 1;
            continue;
        }
        let start = index + 1;
        if !chars.get(start).is_some_and(char::is_ascii_uppercase) {
            output.push('$');
            index += 1;
            continue;
        }
        let mut end = start + 1;
        while chars
            .get(end)
            .is_some_and(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || *ch == '_')
        {
            end += 1;
        }
        let name: String = chars[start..end].iter().collect();
        if let Some(value) = inputs.get(&name) {
            output.push_str(value);
        } else {
            output.extend(chars[index..end].iter());
        }
        index = end;
    }
    output
}

fn expand_numeric_placeholders(content: &str, args: &[String]) -> String {
    let chars: Vec<char> = content.chars().collect();
    let mut output = String::new();
    let mut index = 0;
    let mut joined_args: Option<String> = None;
    while index < chars.len() {
        if chars[index] != '$' {
            output.push(chars[index]);
            index += 1;
            continue;
        }
        match chars.get(index + 1).copied() {
            Some('$') => {
                output.push_str("$$");
                index += 2;
            }
            Some(digit @ '1'..='9') => {
                if let Some(argument) = args.get(digit.to_digit(10).unwrap() as usize - 1) {
                    output.push_str(argument);
                }
                index += 2;
            }
            _ if chars[index + 1..].starts_with(&"ARGUMENTS".chars().collect::<Vec<_>>()) => {
                if !args.is_empty() {
                    let joined = joined_args.get_or_insert_with(|| args.join(" "));
                    output.push_str(joined);
                }
                index += 1 + "ARGUMENTS".chars().count();
            }
            _ => {
                output.push('$');
                index += 1;
            }
        }
    }
    output
}

fn skill_candidates(catalog: Option<&SuggestionCatalog>) -> Vec<Candidate> {
    catalog
        .into_iter()
        .flat_map(|catalog| catalog.skills.iter())
        .filter_map(|skill| {
            let name = skill["name"].as_str()?;
            Some(Candidate {
                id: format!("skill:{name}"),
                label: name.into(),
                detail: skill["description"].as_str().unwrap_or_default().into(),
                insertion: name.into(),
                cursor_offset: None,
            })
        })
        .collect()
}

fn file_candidates(catalog: Option<&SuggestionCatalog>) -> Vec<Candidate> {
    catalog
        .into_iter()
        .flat_map(|catalog| catalog.files.iter())
        .map(|path| Candidate {
            id: format!("file:{path}"),
            label: path.clone(),
            detail: "Workspace file".into(),
            insertion: path.clone(),
            cursor_offset: None,
        })
        .collect()
}

fn runtime_slash_candidates(items: &[Value]) -> Vec<Candidate> {
    items
        .iter()
        .rev()
        .find(|item| item["type"] == "available_commands_update")
        .and_then(|item| {
            item["data"]["availableCommands"]
                .as_array()
                .or_else(|| item["data"]["commands"].as_array())
        })
        .into_iter()
        .flatten()
        .filter_map(|command| {
            let name = command["name"].as_str()?;
            Some(Candidate {
                id: format!("runtime-command:{name}"),
                label: name.trim_start_matches('/').to_owned(),
                detail: command["description"].as_str().unwrap_or_default().into(),
                insertion: name.trim_start_matches('/').to_owned(),
                cursor_offset: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_trigger_and_file_paths_preserve_unicode_boundaries() {
        let draft = "Inspect 🐉 @src/Ж.ts";
        let trigger = active_trigger(draft, draft.len()).unwrap();
        assert_eq!(trigger.character, '@');
        assert_eq!(&draft[trigger.start..trigger.end], "@src/Ж.ts");
    }

    #[test]
    fn prompt_arguments_are_unique_and_skip_argument_blob_marker() {
        assert_eq!(
            prompt_argument_names("$FILE $FILE $ARGUMENTS $2 $$ESCAPED $$$TRIPLE $NAME_2"),
            ["FILE", "NAME_2"]
        );
    }

    #[test]
    fn acp_commands_are_read_only_from_advertised_history() {
        let items = [
            json!({"type":"available_commands_update","data":{"availableCommands":[{"name":"/inspect","description":"inspect"}]}}),
        ];
        let candidates = runtime_slash_candidates(&items);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].label, "inspect");
        assert!(runtime_slash_candidates(&[]).is_empty());
    }

    #[test]
    fn suggestion_navigation_does_not_consume_keys_without_composer_focus() {
        let ctx = egui::Context::default();
        let editor_id = egui::Id::new("unfocused-composer");
        let key = egui::Key::ArrowDown;
        let mut output = ctx.run_ui(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ui| {
                assert!(!consume_suggestion_key(ui.ctx(), editor_id, key));
                assert!(ui.input(|input| input.key_pressed(key)));
            },
        );
        output.textures_delta.clear();
    }

    fn codex_key() -> ConversationKey {
        ConversationKey {
            endpoint_id: "endpoint".into(),
            conversation: ConversationRef::Codex {
                workspace_id: "workspace".into(),
                thread_id: "thread".into(),
            },
        }
    }

    fn acp_key() -> ConversationKey {
        ConversationKey {
            endpoint_id: "endpoint".into(),
            conversation: ConversationRef::Acp {
                workspace_id: "workspace".into(),
                store_id: "store".into(),
                session_id: "session".into(),
            },
        }
    }

    fn catalog_with_prompts(prompts: Value) -> (SuggestionState, ConversationKey) {
        let key = codex_key();
        let mut catalog = SuggestionCatalog::default();
        catalog.prompts = prompts.as_array().cloned().unwrap_or_default();
        catalog.loaded.insert(SuggestionSource::Prompts);
        let mut state = SuggestionState::default();
        state.catalogs.insert(key.clone(), catalog);
        (state, key)
    }

    #[test]
    fn named_prompt_expansion_handles_quotes_unicode_and_escaped_placeholders() {
        let (state, key) = catalog_with_prompts(json!([
            {"name":"named","content":"Hello $NAME; literal $$KEEP."}
        ]));
        assert_eq!(
            state
                .expand_prompt(&key, "/prompts:named NAME=“Cân 🐉 Morgan”")
                .unwrap(),
            "Hello Cân 🐉 Morgan; literal $$KEEP."
        );
    }

    #[test]
    fn named_prompt_expansion_reports_malformed_and_missing_required_inputs() {
        let (state, key) = catalog_with_prompts(json!([
            {"name":"named","content":"Hello $NAME"}
        ]));
        assert_eq!(
            state
                .expand_prompt(&key, "/prompts:named NAME")
                .unwrap_err(),
            "Could not parse /prompts:named: expected key=value but found 'NAME'. Wrap values in double quotes if they contain spaces."
        );
        assert_eq!(
            state
                .expand_prompt(&key, "/prompts:named =value")
                .unwrap_err(),
            "Could not parse /prompts:named: expected a name before '=' in '=value'."
        );
        assert_eq!(
            state.expand_prompt(&key, "/prompts:named").unwrap_err(),
            "Missing required args for /prompts:named: NAME. Provide as key=value (quote values with spaces)."
        );
    }

    #[test]
    fn positional_prompt_expansion_supports_numeric_arguments_blob_and_dollar_escape() {
        let (state, key) = catalog_with_prompts(json!([
            {"name":"positional","content":"1=$1 2=$2 all=$ARGUMENTS escaped=$$ tail=$9"}
        ]));
        assert_eq!(
            state
                .expand_prompt(&key, "/prompts:positional cariad \"two words\"")
                .unwrap(),
            "1=cariad 2=two words all=cariad two words escaped=$$ tail="
        );
    }

    #[test]
    fn expansion_waits_for_codex_catalog_and_leaves_unknown_or_acp_commands_unchanged() {
        let key = codex_key();
        let state = SuggestionState::default();
        assert!(state
            .expand_prompt(&key, "/prompts:known")
            .unwrap_err()
            .contains("still loading"));

        let (state, key) = catalog_with_prompts(json!([]));
        assert_eq!(
            state
                .expand_prompt(&key, "/prompts:unknown NAME=one")
                .unwrap(),
            "/prompts:unknown NAME=one"
        );
        assert_eq!(
            state
                .expand_prompt(&acp_key(), "/prompts:unknown NAME=one")
                .unwrap(),
            "/prompts:unknown NAME=one"
        );
    }

    #[test]
    fn prompt_panel_insert_uses_the_same_argument_template_as_composer_suggestions() {
        assert_eq!(
            prompt_insert_text(&json!({
                "name":"collect",
                "content":"$FIRST then $SECOND and $ARGUMENTS"
            })),
            "/prompts:collect FIRST=\"\" SECOND=\"\""
        );
    }
}
