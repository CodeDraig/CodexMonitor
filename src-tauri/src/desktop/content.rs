//! Native presentation for conversation content and source diffs.
//!
//! The input deliberately stays as JSON: ACP additions must remain inspectable
//! even before the native UI learns a specialized widget for them.

mod diff;
mod markdown;

use std::{
    collections::{HashMap, HashSet},
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    sync::Arc,
    thread,
};

const MAX_MARKDOWN_CACHE_ENTRIES: usize = 2_048;
const MAX_LAYOUT_CACHE_ENTRIES: usize = 512;
const MAX_HEIGHT_CACHE_ENTRIES: usize = 20_000;
const MAX_ATTACHMENT_CACHE_ENTRIES: usize = 256;
const MAX_ATTACHMENT_CACHE_BYTES: usize = 128 * 1024 * 1024;

use egui::{Color32, Context, Pos2, Rect, RichText, ScrollArea, Ui, Vec2};
use serde_json::Value;

use self::markdown::{parse_markdown, parse_markdown_with_source_ranges, MarkdownBlock};

#[derive(Debug, Clone, PartialEq)]
pub enum ContentAction {
    OpenLink {
        url: String,
    },
    OpenFile {
        path: String,
    },
    LoadAttachment {
        id: String,
    },
    ReadAcpTerminal {
        terminal_id: String,
    },
    /// Decode inline data URLs or raw ACP base64 on the host worker.
    LoadInlineAttachment {
        id: String,
        mime: String,
        data: String,
    },
    /// Read a workspace-contained file through the active backend (local or
    /// daemon), then classify its MIME and bytes in the host media worker.
    LoadWorkspaceAttachment {
        id: String,
        path: String,
    },
    PlayAudio {
        id: String,
        mime: String,
        bytes: Vec<u8>,
    },
    Quote {
        item_id: String,
        text: String,
    },
}

#[derive(Debug, Default)]
pub struct ConversationResponse {
    pub actions: Vec<ContentAction>,
    pub visible_range: std::ops::Range<usize>,
    pub scroll_anchor: Option<ScrollAnchor>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ScrollAnchor {
    pub item_id: String,
    /// Vertical distance in points from the visible area's top to the row.
    pub offset: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiffMode {
    #[default]
    Unified,
    Split,
}

#[derive(Debug, Default)]
pub struct DiffResponse {
    pub visible_lines: std::ops::Range<usize>,
}

/// Reusable viewport-rendered diff widget, also suitable for run/Git panels.
pub struct DiffView {
    cache: HashMap<String, diff::CachedDiff>,
    clock: u64,
    worker_tx: Option<SyncSender<DiffJob>>,
    worker_rx: Option<Receiver<DiffResult>>,
    pending: HashMap<String, u64>,
    deferred: HashMap<String, DiffJob>,
    #[cfg(test)]
    completed_parses: usize,
}

struct DiffJob {
    identity: String,
    revision: u64,
    payload: DiffPayload,
    repaint: Context,
}

enum DiffPayload {
    Json(Value),
    Text(String),
}

struct DiffResult {
    identity: String,
    revision: u64,
    model: diff::CachedDiff,
}

impl Default for DiffView {
    fn default() -> Self {
        Self {
            cache: HashMap::new(),
            clock: 0,
            worker_tx: None,
            worker_rx: None,
            pending: HashMap::new(),
            deferred: HashMap::new(),
            #[cfg(test)]
            completed_parses: 0,
        }
    }
}

impl std::fmt::Debug for DiffView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiffView")
            .field("cache_entries", &self.cache.len())
            .field("pending", &self.pending.len())
            .finish()
    }
}

impl DiffView {
    #[cfg(test)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        identity: &str,
        diff: &Value,
        mode: DiffMode,
    ) -> DiffResponse {
        self.clock = self.clock.wrapping_add(1);
        let revision = 0;
        if self
            .cache
            .get(identity)
            .is_none_or(|entry| entry.revision != revision)
        {
            let raw = diff.as_str().unwrap_or("");
            let mut parsed = diff::parse_value(diff, raw);
            parsed.revision = revision;
            diff::insert_cached_diff(&mut self.cache, identity.to_owned(), parsed, self.clock);
        }
        diff::show_diff(&mut self.cache, ui, identity, revision, mode, self.clock)
    }

    /// Versioned production API: each payload revision is parsed off the UI
    /// thread at most once, and results from superseded revisions are ignored.
    pub fn show_versioned(
        &mut self,
        ui: &mut Ui,
        identity: &str,
        value: &Value,
        revision: u64,
        mode: DiffMode,
    ) -> DiffResponse {
        self.show_versioned_with_payload(ui, identity, revision, mode, || {
            DiffPayload::Json(value.clone())
        })
    }

    /// Versioned raw-text variant for tool output and text diffs. The large
    /// borrowed string is copied once only when a new revision is queued.
    pub fn show_text_versioned(
        &mut self,
        ui: &mut Ui,
        identity: &str,
        text: &str,
        revision: u64,
        mode: DiffMode,
    ) -> DiffResponse {
        self.show_versioned_with_payload(ui, identity, revision, mode, || {
            DiffPayload::Text(text.to_owned())
        })
    }

    fn show_versioned_with_payload(
        &mut self,
        ui: &mut Ui,
        identity: &str,
        revision: u64,
        mode: DiffMode,
        make_payload: impl FnOnce() -> DiffPayload,
    ) -> DiffResponse {
        self.clock = self.clock.wrapping_add(1);
        self.ensure_worker();
        if self
            .pending
            .get(identity)
            .is_some_and(|pending_revision| *pending_revision != revision)
        {
            self.deferred.remove(identity);
        }
        self.poll_worker_results();
        if diff::should_prepare(&self.cache, &self.pending, identity, revision) {
            self.deferred.remove(identity);
            if let Some(sender) = self.worker_tx.clone() {
                let job = DiffJob {
                    identity: identity.to_owned(),
                    revision,
                    payload: make_payload(),
                    repaint: ui.ctx().clone(),
                };
                self.pending.insert(identity.to_owned(), revision);
                match sender.try_send(job) {
                    Ok(()) => (),
                    Err(TrySendError::Full(job)) => {
                        if self.deferred.len() < 32 || self.deferred.contains_key(identity) {
                            self.deferred.insert(identity.to_owned(), job);
                        } else {
                            self.pending.remove(identity);
                        }
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(40));
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        self.worker_tx = None;
                        self.pending.remove(identity);
                    }
                }
            }
        }
        diff::show_diff(&mut self.cache, ui, identity, revision, mode, self.clock)
    }

    fn ensure_worker(&mut self) {
        if self.worker_tx.is_some() {
            return;
        }
        let (job_tx, job_rx) = mpsc::sync_channel::<DiffJob>(16);
        let (result_tx, result_rx) = mpsc::channel::<DiffResult>();
        if thread::Builder::new()
            .name("conversation-diff-parser".into())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let mut model = match &job.payload {
                        DiffPayload::Json(value) => {
                            diff::parse_value(value, value.as_str().unwrap_or(""))
                        }
                        DiffPayload::Text(text) => diff::parse_diff(text),
                    };
                    model.revision = job.revision;
                    if result_tx
                        .send(DiffResult {
                            identity: job.identity,
                            revision: job.revision,
                            model,
                        })
                        .is_err()
                    {
                        break;
                    }
                    job.repaint.request_repaint();
                }
            })
            .is_ok()
        {
            self.worker_tx = Some(job_tx);
            self.worker_rx = Some(result_rx);
        }
    }

    fn poll_worker_results(&mut self) {
        let results = self
            .worker_rx
            .as_ref()
            .map(|rx| rx.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        for mut result in results {
            #[cfg(test)]
            {
                self.completed_parses += 1;
            }
            if self.pending.get(&result.identity) != Some(&result.revision) {
                continue;
            }
            self.pending.remove(&result.identity);
            result.model.last_used = self.clock;
            diff::insert_cached_diff(&mut self.cache, result.identity, result.model, self.clock);
        }
        let deferred_keys = self.deferred.keys().cloned().collect::<Vec<_>>();
        for key in deferred_keys {
            let Some(job) = self.deferred.remove(&key) else {
                continue;
            };
            if self.pending.get(&key) != Some(&job.revision) {
                continue;
            }
            let Some(sender) = self.worker_tx.clone() else {
                self.pending.remove(&key);
                continue;
            };
            match sender.try_send(job) {
                Ok(()) => (),
                Err(TrySendError::Full(job)) => {
                    self.deferred.insert(key, job);
                    break;
                }
                Err(TrySendError::Disconnected(_)) => {
                    self.worker_tx = None;
                    self.pending.remove(&key);
                }
            }
        }
    }
}

#[derive(Debug)]
struct CachedItem {
    hash: u64,
    last_used: u64,
    blocks: Vec<MarkdownBlock>,
    source_ranges: markdown::SourceRangeMap,
}

#[derive(Debug)]
struct CachedAudio {
    mime: String,
    bytes: Vec<u8>,
}

struct CachedImageTexture {
    texture: egui::TextureHandle,
    decoded_bytes: usize,
}

#[derive(Debug, Clone)]
enum CachedResourcePreview {
    Text {
        text: String,
        truncated: bool,
        original_base64: Option<Arc<str>>,
    },
    Binary {
        byte_len: usize,
        hex_preview: String,
        decode_note: Option<String>,
        original_base64: Option<Arc<str>>,
    },
}

const MAX_RESOURCE_CACHE_ENTRIES: usize = 256;
const MAX_RESOURCE_CACHE_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone)]
struct CachedResource {
    mime: String,
    preview: CachedResourcePreview,
}

enum RenderJob {
    Markdown {
        key: String,
        hash: u64,
        text: String,
        repaint: Context,
    },
    Highlight {
        key: String,
        code: String,
        language: String,
        fallback: Color32,
        dark: bool,
        repaint: Context,
    },
}

enum RenderResult {
    Markdown {
        key: String,
        hash: u64,
        blocks: Vec<MarkdownBlock>,
        source_ranges: markdown::SourceRangeMap,
    },
    Highlight {
        key: String,
        job: egui::text::LayoutJob,
    },
}

/// Stateful renderer; retain one per main conversation view.
#[derive(Default)]
pub struct ConversationView {
    active_identity: Option<String>,
    cache: HashMap<String, CachedItem>,
    highlighted_code: HashMap<String, egui::text::LayoutJob>,
    diff_view: DiffView,
    heights: HashMap<String, f32>,
    image_textures: HashMap<String, CachedImageTexture>,
    audio_cache: HashMap<String, CachedAudio>,
    resource_cache: HashMap<String, CachedResource>,
    scroll_anchor: Option<(String, f32)>,
    cache_clock: u64,
    diff_revision: u64,
    worker_tx: Option<SyncSender<RenderJob>>,
    worker_rx: Option<Receiver<RenderResult>>,
    pending_markdown: HashMap<String, u64>,
    pending_highlights: HashSet<String>,
}

impl ConversationView {
    /// Resolve a byte selection in a rendered Markdown item's flattened text
    /// stream to UTF-8 byte ranges in its original Markdown source. `item_key`
    /// is the stable key used when rendering that item.
    pub fn source_ranges_for_markdown_selection(
        &self,
        item_key: &str,
        rendered_selection: std::ops::Range<usize>,
    ) -> Option<Vec<std::ops::Range<usize>>> {
        self.cache.get(item_key).map(|cached| {
            cached
                .source_ranges
                .source_ranges_for_selection(rendered_selection)
        })
    }

    fn ensure_worker(&mut self) {
        if self.worker_tx.is_some() {
            return;
        }
        let (job_tx, job_rx) = mpsc::sync_channel::<RenderJob>(64);
        let (result_tx, result_rx) = mpsc::channel::<RenderResult>();
        let worker = thread::Builder::new()
            .name("conversation-content-renderer".into())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    let (result, repaint) = match job {
                        RenderJob::Markdown {
                            key,
                            hash,
                            text,
                            repaint,
                        } => {
                            let parsed = parse_markdown_with_source_ranges(&text);
                            (
                                RenderResult::Markdown {
                                    key,
                                    hash,
                                    blocks: parsed.blocks,
                                    source_ranges: parsed.source_ranges,
                                },
                                repaint,
                            )
                        }
                        RenderJob::Highlight {
                            key,
                            code,
                            language,
                            fallback,
                            dark,
                            repaint,
                        } => (
                            RenderResult::Highlight {
                                key,
                                job: markdown::highlighted_job(&code, &language, fallback, dark),
                            },
                            repaint,
                        ),
                    };
                    if result_tx.send(result).is_err() {
                        break;
                    }
                    repaint.request_repaint();
                }
            });
        if worker.is_ok() {
            self.worker_tx = Some(job_tx);
            self.worker_rx = Some(result_rx);
        }
    }

    fn poll_worker_results(&mut self) {
        let results = self
            .worker_rx
            .as_ref()
            .map(|receiver| receiver.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        for result in results {
            match result {
                RenderResult::Markdown {
                    key,
                    hash,
                    blocks,
                    source_ranges,
                } => {
                    if self.pending_markdown.get(&key) == Some(&hash) {
                        self.pending_markdown.remove(&key);
                        if self.cache.len() >= MAX_MARKDOWN_CACHE_ENTRIES {
                            if let Some(evict) = self
                                .cache
                                .iter()
                                .min_by_key(|(_, entry)| entry.last_used)
                                .map(|(key, _)| key.clone())
                            {
                                self.cache.remove(&evict);
                            }
                        }
                        self.cache.insert(
                            key,
                            CachedItem {
                                hash,
                                last_used: self.cache_clock,
                                blocks,
                                source_ranges,
                            },
                        );
                    }
                }
                RenderResult::Highlight { key, job } => {
                    if self.pending_highlights.remove(&key) {
                        if self.highlighted_code.len() >= MAX_LAYOUT_CACHE_ENTRIES {
                            if let Some(evict) = self.highlighted_code.keys().next().cloned() {
                                self.highlighted_code.remove(&evict);
                            }
                        }
                        self.highlighted_code.insert(key, job);
                    }
                }
            }
        }
    }

    fn request_markdown(&mut self, ctx: &Context, key: &str, text: &str, hash: u64) {
        if self.pending_markdown.get(key) == Some(&hash) {
            return;
        }
        self.pending_markdown.remove(key);
        self.ensure_worker();
        let Some(sender) = &self.worker_tx else {
            return;
        };
        let job = RenderJob::Markdown {
            key: key.to_owned(),
            hash,
            text: text.to_owned(),
            repaint: ctx.clone(),
        };
        if sender.try_send(job).is_ok() {
            self.pending_markdown.insert(key.to_owned(), hash);
        }
    }

    fn request_highlight(
        &mut self,
        ctx: &Context,
        key: &str,
        code: &str,
        language: &str,
        fallback: Color32,
        dark: bool,
    ) {
        if self.pending_highlights.contains(key) {
            return;
        }
        self.ensure_worker();
        let Some(sender) = &self.worker_tx else {
            return;
        };
        let job = RenderJob::Highlight {
            key: key.to_owned(),
            code: code.to_owned(),
            language: language.to_owned(),
            fallback,
            dark,
            repaint: ctx.clone(),
        };
        if sender.try_send(job).is_ok() {
            self.pending_highlights.insert(key.to_owned());
        }
    }

    fn cache_markdown_hit(&mut self, key: &str, hash: u64) -> Option<Vec<MarkdownBlock>> {
        self.cache.get_mut(key).and_then(|cached| {
            if cached.hash == hash {
                cached.last_used = self.cache_clock;
                Some(cached.blocks.clone())
            } else {
                None
            }
        })
    }

    /// Cache already-decoded RGBA attachment data for native image rendering.
    /// The host is responsible for loading/decoding bytes off the UI thread.
    pub fn cache_image(
        &mut self,
        ctx: &Context,
        conversation_identity: &str,
        attachment_id: impl Into<String>,
        width: usize,
        height: usize,
        rgba: &[u8],
    ) {
        let id = media_key(conversation_identity, &attachment_id.into());
        if !make_attachment_cache_room(
            &mut self.image_textures,
            &mut self.audio_cache,
            &id,
            rgba.len(),
        ) {
            return;
        }
        let image = egui::ColorImage::from_rgba_unmultiplied([width, height], rgba);
        self.image_textures.insert(
            id.clone(),
            CachedImageTexture {
                texture: ctx.load_texture(id, image, egui::TextureOptions::LINEAR),
                decoded_bytes: rgba.len(),
            },
        );
    }

    pub fn forget_image(&mut self, conversation_identity: &str, attachment_id: &str) {
        self.image_textures
            .remove(&media_key(conversation_identity, attachment_id));
    }

    /// Retain bytes delivered by the host's attachment loader. Actual audio
    /// playback stays in the host audio service and is requested by action.
    pub fn cache_audio(
        &mut self,
        conversation_identity: &str,
        attachment_id: impl Into<String>,
        mime: impl Into<String>,
        bytes: Vec<u8>,
    ) {
        let key = media_key(conversation_identity, &attachment_id.into());
        let mime = mime.into();
        if !make_attachment_cache_room(
            &mut self.image_textures,
            &mut self.audio_cache,
            &key,
            bytes.len().saturating_add(mime.len()),
        ) {
            return;
        }
        self.audio_cache.insert(key, CachedAudio { mime, bytes });
    }

    pub fn forget_audio(&mut self, conversation_identity: &str, attachment_id: &str) {
        self.audio_cache
            .remove(&media_key(conversation_identity, attachment_id));
    }

    pub fn cache_resource_text(
        &mut self,
        conversation_identity: &str,
        attachment_id: impl Into<String>,
        mime: impl Into<String>,
        text: String,
        truncated: bool,
    ) {
        self.cache_resource_text_inner(
            conversation_identity,
            attachment_id.into(),
            mime.into(),
            text,
            truncated,
            None,
        );
    }

    pub fn cache_resource_text_original(
        &mut self,
        conversation_identity: &str,
        attachment_id: impl Into<String>,
        mime: impl Into<String>,
        text: String,
        truncated: bool,
        original_base64: String,
    ) {
        self.cache_resource_text_inner(
            conversation_identity,
            attachment_id.into(),
            mime.into(),
            text,
            truncated,
            Some(Arc::<str>::from(original_base64)),
        );
    }

    fn cache_resource_text_inner(
        &mut self,
        conversation_identity: &str,
        attachment_id: String,
        mime: String,
        text: String,
        truncated: bool,
        original_base64: Option<Arc<str>>,
    ) {
        self.insert_resource_preview(
            conversation_identity,
            attachment_id,
            CachedResource {
                mime,
                preview: CachedResourcePreview::Text {
                    text,
                    truncated,
                    original_base64,
                },
            },
        );
    }

    pub fn cache_resource_binary(
        &mut self,
        conversation_identity: &str,
        attachment_id: impl Into<String>,
        mime: impl Into<String>,
        byte_len: usize,
        hex_preview: String,
        decode_note: Option<String>,
    ) {
        self.cache_resource_binary_inner(
            conversation_identity,
            attachment_id.into(),
            mime.into(),
            byte_len,
            hex_preview,
            decode_note,
            None,
        );
    }

    pub fn cache_resource_binary_original(
        &mut self,
        conversation_identity: &str,
        attachment_id: impl Into<String>,
        mime: impl Into<String>,
        byte_len: usize,
        hex_preview: String,
        decode_note: Option<String>,
        original_base64: String,
    ) {
        self.cache_resource_binary_inner(
            conversation_identity,
            attachment_id.into(),
            mime.into(),
            byte_len,
            hex_preview,
            decode_note,
            Some(Arc::<str>::from(original_base64)),
        );
    }

    fn cache_resource_binary_inner(
        &mut self,
        conversation_identity: &str,
        attachment_id: String,
        mime: String,
        byte_len: usize,
        hex_preview: String,
        decode_note: Option<String>,
        original_base64: Option<Arc<str>>,
    ) {
        self.insert_resource_preview(
            conversation_identity,
            attachment_id,
            CachedResource {
                mime,
                preview: CachedResourcePreview::Binary {
                    byte_len,
                    hex_preview,
                    decode_note,
                    original_base64,
                },
            },
        );
    }

    fn insert_resource_preview(
        &mut self,
        conversation_identity: &str,
        attachment_id: String,
        mut resource: CachedResource,
    ) {
        let key = media_key(conversation_identity, &attachment_id);
        insert_cached_resource(
            &mut self.resource_cache,
            key,
            resource,
            MAX_RESOURCE_CACHE_ENTRIES,
            MAX_RESOURCE_CACHE_BYTES,
        );
    }

    /// Render a virtualized, ordered conversation. Only visible rows are parsed
    /// on first display; stable identity and item id retain layout estimates.
    pub fn show(&mut self, ui: &mut Ui, items: &[Value], identity: &str) -> ConversationResponse {
        self.show_with_revision(ui, items, identity, 0)
    }

    pub fn show_with_revision(
        &mut self,
        ui: &mut Ui,
        items: &[Value],
        identity: &str,
        items_revision: u64,
    ) -> ConversationResponse {
        self.diff_revision = items_revision;
        if self.active_identity.as_deref() != Some(identity) {
            self.active_identity = Some(identity.to_owned());
            self.cache.clear();
            self.highlighted_code.clear();
            self.diff_view.cache.clear();
            self.heights.clear();
            self.image_textures.clear();
            self.audio_cache.clear();
            self.resource_cache.clear();
            self.scroll_anchor = None;
            self.pending_markdown.clear();
        }
        self.poll_worker_results();
        let mut response = ConversationResponse::default();
        let mut prefix = Vec::with_capacity(items.len() + 1);
        prefix.push(0.0f32);
        for item in items {
            let key = item_key(identity, item);
            let h = self.heights.get(&key).copied().unwrap_or(58.0).max(24.0);
            prefix.push(prefix.last().copied().unwrap_or_default() + h + 8.0);
        }

        let width = ui.available_width().max(1.0);
        let anchor = self.scroll_anchor.clone();
        let mut restored_anchor = false;
        let scroll = ScrollArea::vertical()
            .id_salt(("native-conversation", identity))
            .auto_shrink([false, false]);
        let area = scroll.show_viewport(ui, |ui, viewport| {
            // `viewport` is relative to the content origin, while `ui.cursor()`
            // is in screen coordinates and moves as the scroll offset changes.
            // Keep the origin so virtual padding and row anchors stay in the
            // same coordinate space as the prefix table.
            let content_origin = ui.cursor().min.y;
            let top = viewport.min.y.max(0.0);
            let bottom = viewport.max.y.max(top);
            let range = visible_range(&prefix, items.len(), top, bottom);
            let mut start = range.start;
            let mut end = range.end;
            let anchor_index = anchor.as_ref().and_then(|(anchor_key, _)| {
                items.iter().position(|item| {
                    item_key(identity, item) == *anchor_key
                        || item["id"].as_str() == Some(anchor_key.as_str())
                })
            });
            if let Some(anchor_index) = anchor_index {
                let visible_count = end.saturating_sub(start).max(1);
                start = anchor_index;
                end = (anchor_index + visible_count).min(items.len());
            }
            if start < items.len() && end <= start {
                end = (start + 1).min(items.len());
            }
            response.visible_range = start..end;
            if start > 0 {
                ui.add_space(prefix[start]);
            }
            for (index, item) in items.iter().enumerate().take(end).skip(start) {
                let key = item_key(identity, item);
                let before = ui.cursor().min.y;
                if index == start {
                    response.scroll_anchor = item["id"].as_str().map(|item_id| ScrollAnchor {
                        item_id: item_id.to_owned(),
                        offset: viewport_row_offset(before, content_origin, viewport.min.y),
                    });
                }
                if let Some((anchor_key, offset)) = &anchor {
                    if key == *anchor_key || item["id"].as_str() == Some(anchor_key.as_str()) {
                        ui.scroll_to_rect(
                            Rect::from_min_size(
                                Pos2::new(ui.min_rect().left(), before - offset),
                                Vec2::new(width, 1.0),
                            ),
                            Some(egui::Align::Min),
                        );
                        restored_anchor = true;
                    }
                }
                self.show_item(ui, item, &key, &mut response.actions);
                let measured = (ui.cursor().min.y - before).max(24.0);
                cache_height(&mut self.heights, key.clone(), measured);
                let gap = 8.0;
                if index + 1 < items.len() {
                    ui.add_space(gap);
                }
            }
            let laid_out_height = laid_out_content_height(ui.cursor().min.y, content_origin);
            let total = prefix.last().copied().unwrap_or_default();
            if total > laid_out_height {
                ui.add_space(total - laid_out_height);
            }
            let _ = width;
        });

        if restored_anchor {
            self.scroll_anchor = None;
        }
        // ScrollArea retains the actual position as row-height estimates converge.
        let _ = area;
        response
    }

    /// Remember the first visible item and its in-row offset before replacing a
    /// history generation. The shell can call this before applying a replay.
    pub fn set_scroll_anchor(&mut self, item_id: impl Into<String>, offset: f32) {
        self.scroll_anchor = Some((item_id.into(), offset));
    }

    pub fn clear_scroll_anchor(&mut self) {
        self.scroll_anchor = None;
    }

    fn show_item(
        &mut self,
        ui: &mut Ui,
        item: &Value,
        key: &str,
        actions: &mut Vec<ContentAction>,
    ) {
        let kind = item["kind"].as_str().unwrap_or("unknown");
        let id = item["id"].as_str().unwrap_or("unknown-item").to_owned();
        match kind {
            "message" => {
                let role = item["role"].as_str().unwrap_or("assistant");
                let text = item["text"].as_str().unwrap_or("");
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(if role == "user" { "You" } else { "Agent" })
                            .strong()
                            .color(if role == "user" {
                                Color32::from_rgb(133, 173, 255)
                            } else {
                                Color32::from_rgb(139, 211, 175)
                            }),
                    );
                    if ui.small_button("Quote").clicked() {
                        actions.push(ContentAction::Quote {
                            item_id: id.clone(),
                            text: text.to_owned(),
                        });
                    }
                });
                self.render_markdown(ui, text, key, actions);
                let content_blocks = message_content_blocks(
                    &item["content"],
                    text.trim().is_empty(),
                    item["images"]
                        .as_array()
                        .is_some_and(|images| !images.is_empty()),
                );
                self.render_content_blocks(ui, &content_blocks, key, actions);
                self.render_content_blocks(ui, &item["acpContent"]["blocks"], key, actions);
                let image_blocks = codex_image_blocks(&item["images"]);
                self.render_content_blocks(ui, &Value::Array(image_blocks), key, actions);
            }
            "reasoning" => {
                let title = item["summary"].as_str().unwrap_or("Reasoning");
                egui::CollapsingHeader::new(title)
                    .id_salt(key)
                    .default_open(false)
                    .show(ui, |ui| {
                        let text = item["content"]
                            .as_str()
                            .or(item["text"].as_str())
                            .unwrap_or("");
                        self.render_markdown(ui, text, key, actions);
                        self.render_content_blocks(ui, &item["content"], key, actions);
                        self.render_content_blocks(ui, &item["acpContent"]["blocks"], key, actions);
                    });
            }
            "diff" => {
                ui.label(RichText::new(item["title"].as_str().unwrap_or("Change")).strong());
                let _ = self.diff_view.show_versioned(
                    ui,
                    key,
                    &item["diff"],
                    self.diff_revision,
                    DiffMode::Unified,
                );
            }
            "review" => {
                let state = match item["state"].as_str().unwrap_or("") {
                    "started" => "Review started",
                    "completed" => "Review completed",
                    other if other.is_empty() => "Review activity",
                    other => other,
                };
                ui.label(RichText::new(state).strong());
                if let Some(text) = item["text"].as_str().filter(|text| !text.is_empty()) {
                    self.render_markdown(ui, text, key, actions);
                }
            }
            "tool" => {
                let title = item["title"].as_str().unwrap_or("Tool activity");
                let is_compaction = item["toolType"] == "acp-compaction"
                    || title.to_ascii_lowercase().contains("compaction");
                let status = item["status"].as_str().unwrap_or("");
                egui::CollapsingHeader::new(format!(
                    "{title}{}",
                    if status.is_empty() {
                        String::new()
                    } else {
                        format!(" · {status}")
                    }
                ))
                .id_salt(key)
                .default_open(is_compaction)
                .show(ui, |ui| {
                    let has_terminal_content = contains_terminal_content(&item["content"])
                        || contains_terminal_content(&item["acpContent"]["blocks"]);
                    if !has_terminal_content {
                        if let Some(terminal_id) = item["terminalId"]
                            .as_str()
                            .or(item.pointer("/terminal/id").and_then(Value::as_str))
                        {
                            if ui.button("Read command terminal output").clicked() {
                                actions.push(ContentAction::ReadAcpTerminal {
                                    terminal_id: terminal_id.to_owned(),
                                });
                            }
                        }
                    }
                    if let Some(detail) = item["detail"].as_str().filter(|s| !s.is_empty()) {
                        ui.label(RichText::new("Input").strong());
                        self.render_code(ui, detail, "json");
                    }
                    if let Some(output) = item["output"].as_str().filter(|s| !s.is_empty()) {
                        if file_change_has_separate_diffs(item) {
                            ui.collapsing("Combined output (raw)", |ui| {
                                ui.add(egui::Label::new(output).selectable(true));
                            });
                        } else if looks_like_diff(output) {
                            ui.label(RichText::new("Output").strong());
                            let _ = self.diff_view.show_text_versioned(
                                ui,
                                &format!("{key}/output"),
                                output,
                                self.diff_revision,
                                DiffMode::Unified,
                            );
                        } else {
                            ui.label(RichText::new("Output").strong());
                            self.render_markdown(ui, output, key, actions);
                        }
                    }
                    if let Some(changes) = item["changes"].as_array() {
                        for change in changes {
                            ui.label(
                                RichText::new(change["path"].as_str().unwrap_or("Changed file"))
                                    .strong(),
                            );
                            let identity =
                                format!("{key}/{}", change["path"].as_str().unwrap_or("change"));
                            if let Some(patch) = change["diff"].as_str() {
                                let _ = self.diff_view.show_text_versioned(
                                    ui,
                                    &identity,
                                    patch,
                                    self.diff_revision,
                                    DiffMode::Unified,
                                );
                            } else {
                                let _ = self.diff_view.show_versioned(
                                    ui,
                                    &identity,
                                    &change["diff"],
                                    self.diff_revision,
                                    DiffMode::Unified,
                                );
                            }
                        }
                    }
                    self.render_content_blocks(ui, &item["content"], key, actions);
                    self.render_content_blocks(ui, &item["acpContent"]["blocks"], key, actions);
                });
            }
            _ => {
                egui::CollapsingHeader::new(format!(
                    "{} · supplied content",
                    item["type"].as_str().unwrap_or(kind)
                ))
                .id_salt(key)
                .show(ui, |ui| {
                    self.render_code(
                        ui,
                        &serde_json::to_string_pretty(item).unwrap_or_default(),
                        "json",
                    )
                });
            }
        }
    }

    fn render_markdown(
        &mut self,
        ui: &mut Ui,
        text: &str,
        key: &str,
        actions: &mut Vec<ContentAction>,
    ) {
        self.cache_clock = self.cache_clock.wrapping_add(1);
        let hash = hash_text(text);
        let Some(blocks) = self.cache_markdown_hit(key, hash) else {
            self.request_markdown(ui.ctx(), key, text, hash);
            ui.add(egui::Label::new(text).selectable(true));
            return;
        };
        for (index, block) in blocks.iter().enumerate() {
            match block {
                MarkdownBlock::Paragraph(spans) => {
                    self.render_spans(ui, spans, None, actions);
                }
                MarkdownBlock::Heading { level, spans } => {
                    let size = match level {
                        1 => 24.0,
                        2 => 21.0,
                        3 => 18.0,
                        _ => 16.0,
                    };
                    self.render_spans(ui, spans, Some(size), actions);
                }
                MarkdownBlock::Code { language, text } => {
                    self.render_code(ui, text, language.as_deref().unwrap_or(""))
                }
                MarkdownBlock::Quote(spans) => {
                    egui::Frame::new()
                        .fill(ui.visuals().faint_bg_color)
                        .inner_margin(8.0)
                        .show(ui, |ui| {
                            self.render_spans(ui, spans, None, actions);
                        });
                }
                MarkdownBlock::Table(rows) => {
                    let copy = rows
                        .iter()
                        .map(|row| row.join("\t"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if ui.small_button("Copy table text").clicked() {
                        ui.ctx().copy_text(copy);
                    }
                    egui::Grid::new((key, "table", index))
                        .striped(true)
                        .show(ui, |ui| {
                            for (row_i, row) in rows.iter().enumerate() {
                                for cell in row {
                                    ui.add(
                                        egui::Label::new(RichText::new(cell).strong().size(13.0))
                                            .selectable(true),
                                    );
                                }
                                let _ = row_i;
                                ui.end_row();
                            }
                        });
                }
                MarkdownBlock::Rule => {
                    ui.separator();
                }
            }
        }
    }

    fn render_spans(
        &mut self,
        ui: &mut Ui,
        spans: &[markdown::Span],
        size: Option<f32>,
        actions: &mut Vec<ContentAction>,
    ) {
        let mut plain = String::new();
        let mut links = Vec::new();
        for span in spans {
            plain.push_str(&span.text);
            if let Some(url) = &span.link {
                if !links
                    .iter()
                    .any(|(label, target): &(String, String)| label == &span.text && target == url)
                {
                    links.push((span.text.clone(), url.clone()));
                }
            }
        }
        let job = spans_layout_job(ui.style(), spans, size);
        ui.add(egui::Label::new(job).selectable(true))
            .context_menu(|ui| {
                if ui.button("Copy text").clicked() {
                    ui.ctx().copy_text(plain.clone());
                    ui.close();
                }
            });
        if !links.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for (label, url) in links {
                    if ui.small_button(format!("↗ {}", label.trim())).clicked() {
                        actions.push(link_action(&url));
                    }
                }
            });
        }
    }

    fn render_code(&mut self, ui: &mut Ui, code: &str, language: &str) {
        let dark = ui.visuals().dark_mode;
        let cache_key = format!("{language}:{dark}:{}", hash_text(code));
        if !self.highlighted_code.contains_key(&cache_key) {
            self.request_highlight(
                ui.ctx(),
                &cache_key,
                code,
                language,
                ui.visuals().text_color(),
                dark,
            );
            ui.add(egui::Label::new(RichText::new(code).monospace()).selectable(true));
            return;
        }
        let mut job = self.highlighted_code[&cache_key].clone();
        let code_font = egui::TextStyle::Monospace.resolve(ui.style());
        for section in &mut job.sections {
            section.format.font_id = code_font.clone();
        }
        egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .inner_margin(8.0)
            .show(ui, |ui| {
                ui.add(egui::Label::new(job).selectable(true));
            });
    }

    fn render_content_blocks(
        &mut self,
        ui: &mut Ui,
        blocks: &Value,
        item_key: &str,
        actions: &mut Vec<ContentAction>,
    ) {
        let Some(blocks) = blocks.as_array() else {
            return;
        };
        let conversation_identity = item_key
            .rsplit_once('/')
            .map(|(identity, _)| identity)
            .unwrap_or(item_key);
        for (i, block) in blocks.iter().enumerate() {
            let content = block
                .get("content")
                .filter(|value| value.is_object())
                .unwrap_or(block);
            let kind = content["type"]
                .as_str()
                .or(block["type"].as_str())
                .unwrap_or("content");
            match kind {
                "text" => {
                    if let Some(text) = content["text"].as_str() {
                        self.render_markdown(ui, text, item_key, actions);
                    }
                }
                "plan" | "agent_plan" => {
                    let title = content["title"].as_str().unwrap_or("Agent plan");
                    egui::CollapsingHeader::new(title)
                        .default_open(true)
                        .id_salt((item_key, "plan", i))
                        .show(ui, |ui| {
                            if let Some(text) =
                                content["text"].as_str().or(content["content"].as_str())
                            {
                                self.render_markdown(ui, text, item_key, actions);
                            } else {
                                self.render_code(
                                    ui,
                                    &serde_json::to_string_pretty(content).unwrap_or_default(),
                                    "json",
                                );
                            }
                        });
                }
                "configuration" | "config" | "config_option_update" => {
                    let title = content["title"]
                        .as_str()
                        .or(content["optionId"].as_str())
                        .unwrap_or("Agent configuration");
                    ui.label(RichText::new(title).strong());
                    self.render_code(
                        ui,
                        &serde_json::to_string_pretty(content).unwrap_or_default(),
                        "json",
                    );
                }
                "compaction" | "context_compaction" => {
                    ui.label(RichText::new("Context compaction").strong());
                    if let Some(summary) = content["summary"].as_str() {
                        self.render_markdown(ui, summary, item_key, actions);
                    } else {
                        self.render_code(
                            ui,
                            &serde_json::to_string_pretty(content).unwrap_or_default(),
                            "json",
                        );
                    }
                }
                "image" | "localImage" => {
                    let id = content["attachmentId"]
                        .as_str()
                        .or(content["id"].as_str())
                        .or(content["uri"].as_str())
                        .or(content["path"].as_str())
                        .unwrap_or("")
                        .to_owned();
                    let scoped_id = media_key(conversation_identity, &id);
                    if let Some(texture) = self.image_textures.get(&scoped_id) {
                        ui.add(
                            egui::Image::new(&texture.texture).max_size(Vec2::new(640.0, 480.0)),
                        );
                    } else if let Some(preview) = self.resource_cache.get(&scoped_id).cloned() {
                        self.render_resource_preview(ui, &preview, item_key, actions);
                    } else if let Some(action) = attachment_action(content, &id) {
                        if ui.button(format!("Load image {}", i + 1)).clicked() {
                            actions.push(action);
                        }
                    } else if let Some(url) = content["url"]
                        .as_str()
                        .filter(|url| url.starts_with("https://") || url.starts_with("http://"))
                    {
                        ui.label("External image URL");
                        if ui.link(url).clicked() {
                            actions.push(ContentAction::OpenLink {
                                url: url.to_owned(),
                            });
                        }
                    } else {
                        ui.label("Image content is unavailable.");
                        egui::CollapsingHeader::new("Image metadata").show(ui, |ui| {
                            self.render_code(
                                ui,
                                &serde_json::to_string_pretty(content).unwrap_or_default(),
                                "json",
                            );
                        });
                    }
                }
                "audio" | "audio_file" => {
                    let id = content["attachmentId"]
                        .as_str()
                        .or(content["id"].as_str())
                        .or(content["uri"].as_str())
                        .or(content["path"].as_str())
                        .unwrap_or("")
                        .to_owned();
                    let scoped_id = media_key(conversation_identity, &id);
                    if self.audio_cache.contains_key(&scoped_id) {
                        if ui
                            .button(format!(
                                "Play audio · {} bytes",
                                self.audio_cache[&scoped_id].bytes.len()
                            ))
                            .clicked()
                        {
                            let audio = &self.audio_cache[&scoped_id];
                            actions.push(ContentAction::PlayAudio {
                                id: id.clone(),
                                mime: audio.mime.clone(),
                                bytes: audio.bytes.clone(),
                            });
                        }
                    } else if let Some(action) = attachment_action(content, &id) {
                        if ui
                            .button(format!(
                                "Load audio · {}",
                                content["mimeType"].as_str().unwrap_or("media")
                            ))
                            .clicked()
                        {
                            actions.push(action);
                        }
                    } else {
                        ui.label("Audio content is unavailable.");
                        egui::CollapsingHeader::new("Audio metadata").show(ui, |ui| {
                            self.render_code(
                                ui,
                                &serde_json::to_string_pretty(content).unwrap_or_default(),
                                "json",
                            );
                        });
                    }
                }
                "terminal" => {
                    if let Some(terminal_id) = content["terminalId"].as_str() {
                        ui.label(RichText::new("Command terminal").strong());
                        if ui.button("Read command terminal output").clicked() {
                            let action = ContentAction::ReadAcpTerminal {
                                terminal_id: terminal_id.to_owned(),
                            };
                            if !actions.contains(&action) {
                                actions.push(action);
                            }
                        }
                    } else {
                        self.render_code(
                            ui,
                            &serde_json::to_string_pretty(content).unwrap_or_default(),
                            "json",
                        );
                    }
                }
                "resource_link" | "link" => {
                    let url = content["uri"]
                        .as_str()
                        .or(content["url"].as_str())
                        .unwrap_or("");
                    let label = content["name"]
                        .as_str()
                        .or(content["title"].as_str())
                        .unwrap_or(url);
                    if let Some(action) = attachment_action(content, url) {
                        if ui
                            .button(format!(
                                "Load resource · {}",
                                content["mimeType"].as_str().unwrap_or("file")
                            ))
                            .clicked()
                        {
                            actions.push(action);
                        }
                    } else if !url.is_empty() && ui.link(label).clicked() {
                        actions.push(link_action(url));
                    }
                }
                "resource" => {
                    let resource = content
                        .get("resource")
                        .filter(|value| value.is_object())
                        .unwrap_or(content);
                    let name = resource["name"]
                        .as_str()
                        .or(resource["uri"].as_str())
                        .unwrap_or("Embedded resource");
                    ui.label(RichText::new(name).strong());
                    let resource_id = resource["attachmentId"]
                        .as_str()
                        .or(resource["id"].as_str())
                        .or(resource["uri"].as_str())
                        .or(resource["path"].as_str())
                        .unwrap_or("");
                    let resource_key = media_key(conversation_identity, resource_id);
                    if let Some(text) = resource["text"].as_str() {
                        ui.add(egui::Label::new(text).selectable(true))
                            .context_menu(|ui| {
                                if ui.button("Copy text").clicked() {
                                    ui.ctx().copy_text(text.to_owned());
                                    ui.close();
                                }
                            });
                    } else if let Some(preview) = self.resource_cache.get(&resource_key).cloned() {
                        self.render_resource_preview(ui, &preview, item_key, actions);
                    } else if let Some(action) = attachment_action(resource, resource_id) {
                        if ui
                            .link(format!(
                                "Load embedded resource · {}",
                                resource["mimeType"].as_str().unwrap_or("file")
                            ))
                            .clicked()
                        {
                            actions.push(action);
                        }
                    } else {
                        ui.label("Resource content supplied by the agent.");
                        egui::CollapsingHeader::new("Resource metadata").show(ui, |ui| {
                            self.render_code(
                                ui,
                                &serde_json::to_string_pretty(block).unwrap_or_default(),
                                "json",
                            )
                        });
                    }
                }
                "diff" => {
                    let _ = self.diff_view.show_versioned(
                        ui,
                        &format!("{item_key}/content-block/{i}"),
                        content.get("diff").unwrap_or(content),
                        self.diff_revision,
                        DiffMode::Unified,
                    );
                }
                _ => {
                    egui::CollapsingHeader::new(format!("{} content", kind)).show(ui, |ui| {
                        let id = content["attachmentId"]
                            .as_str()
                            .or(content["id"].as_str())
                            .or(content["uri"].as_str())
                            .or(content["path"].as_str())
                            .unwrap_or("");
                        let scoped_id = media_key(conversation_identity, id);
                        if let Some(preview) = self.resource_cache.get(&scoped_id).cloned() {
                            self.render_resource_preview(ui, &preview, item_key, actions);
                        } else if let Some(action) = attachment_action(content, id) {
                            if ui.button(format!("Load {kind} content")).clicked() {
                                actions.push(action);
                            }
                            self.render_code(
                                ui,
                                &serde_json::to_string_pretty(block).unwrap_or_default(),
                                "json",
                            );
                        } else {
                            self.render_code(
                                ui,
                                &serde_json::to_string_pretty(block).unwrap_or_default(),
                                "json",
                            );
                        }
                    });
                }
            }
        }
    }

    fn render_resource_preview(
        &mut self,
        ui: &mut Ui,
        resource: &CachedResource,
        item_key: &str,
        actions: &mut Vec<ContentAction>,
    ) {
        ui.label(
            RichText::new(format!("Loaded · {}", resource.mime))
                .small()
                .strong(),
        );
        match &resource.preview {
            CachedResourcePreview::Text {
                text,
                truncated,
                original_base64,
            } => {
                if *truncated {
                    ui.label(
                        RichText::new("Preview truncated at 1 MiB")
                            .small()
                            .color(Color32::GRAY),
                    );
                }
                self.render_markdown(ui, text, &format!("{item_key}/loaded-resource"), actions);
                if let Some(original_base64) = original_base64 {
                    if ui.button("Copy complete resource as base64").clicked() {
                        ui.ctx().copy_text(original_base64.to_string());
                    }
                } else if *truncated {
                    ui.label("Original bytes were not retained for copying.");
                }
            }
            CachedResourcePreview::Binary {
                byte_len,
                hex_preview,
                decode_note,
                original_base64,
            } => {
                ui.label(format!(
                    "{byte_len} bytes · binary preview (first 96 bytes at most)"
                ));
                if let Some(note) = decode_note {
                    ui.colored_label(Color32::LIGHT_RED, note);
                }
                if !hex_preview.is_empty() {
                    ui.add(
                        egui::Label::new(RichText::new(hex_preview).monospace()).selectable(true),
                    )
                    .context_menu(|ui| {
                        if ui.button("Copy byte preview").clicked() {
                            ui.ctx().copy_text(hex_preview.clone());
                            ui.close();
                        }
                    });
                }
                if let Some(original_base64) = original_base64 {
                    if ui.button("Copy original as base64").clicked() {
                        ui.ctx().copy_text(original_base64.to_string());
                    }
                } else {
                    ui.label("Original bytes were not retained for copying.");
                }
            }
        }
    }
}

fn styled_text(
    text: &str,
    strong: bool,
    emphasis: bool,
    strikethrough: bool,
    code: bool,
    link: bool,
) -> RichText {
    let mut rich = RichText::new(text.to_owned());
    if strong {
        rich = rich.strong();
    }
    if emphasis {
        rich = rich.italics();
    }
    if strikethrough {
        rich = rich.strikethrough();
    }
    if code {
        rich = rich.monospace().background_color(Color32::from_gray(43));
    }
    if link {
        rich = rich.underline().color(Color32::from_rgb(117, 174, 255));
    }
    rich
}

fn spans_layout_job(
    style: &egui::Style,
    spans: &[markdown::Span],
    size: Option<f32>,
) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    for span in spans {
        let mut rich = styled_text(
            &span.text,
            span.strong,
            span.emphasis,
            span.strikethrough,
            span.code,
            span.link.is_some(),
        );
        if let Some(size) = size {
            rich = rich.size(size);
        }
        rich.append_to(
            &mut job,
            style,
            egui::FontSelection::Default,
            egui::Align::BOTTOM,
        );
    }
    job
}

fn link_action(url: &str) -> ContentAction {
    if let Some(path) = url.strip_prefix("workspace://") {
        ContentAction::OpenFile {
            path: path.to_owned(),
        }
    } else if let Some(path) = url.strip_prefix("file://") {
        ContentAction::OpenFile {
            path: path.to_owned(),
        }
    } else {
        ContentAction::OpenLink {
            url: url.to_owned(),
        }
    }
}

fn attachment_action(content: &Value, id: &str) -> Option<ContentAction> {
    let mime = content["mimeType"]
        .as_str()
        .or(content["mime_type"].as_str())
        .unwrap_or("application/octet-stream")
        .to_owned();
    if let Some(data) = content["data"].as_str().or(content["blob"].as_str()) {
        return Some(ContentAction::LoadInlineAttachment {
            id: if id.is_empty() {
                format!("inline-{:x}", hash_text(data))
            } else {
                id.to_owned()
            },
            mime,
            data: data.to_owned(),
        });
    }
    if let Some(attachment_id) = content["attachmentId"].as_str() {
        return Some(ContentAction::LoadAttachment {
            id: attachment_id.to_owned(),
        });
    }
    let uri = content["uri"].as_str().or(content["url"].as_str());
    let path = if let Some(path) = content["path"].as_str() {
        path.to_owned()
    } else {
        let uri = uri?;
        if let Some(path) = uri.strip_prefix("workspace://") {
            decode_uri_path(path)
        } else if let Some(path) = uri.strip_prefix("file://") {
            let path = decode_uri_path(path);
            // `file://host/path` is an authority-bearing URI, not a local
            // absolute path. Do not reinterpret its host as a workspace file.
            if !path.starts_with('/') {
                return None;
            }
            path
        } else {
            return None;
        }
    };
    let fallback_id = format!("path-{:x}", hash_text(&path));
    Some(ContentAction::LoadWorkspaceAttachment {
        id: if id.is_empty() {
            fallback_id
        } else {
            id.to_owned()
        },
        path,
    })
}

/// The shared Rust controller keeps Codex's normal frontend-compatible
/// `images: string[]` projection. Convert it to the same attachment block shape
/// consumed by ACP so image loading and backend scoping stay centralized.
fn codex_image_blocks(images: &Value) -> Vec<Value> {
    images
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|source| {
            if source.starts_with("data:") {
                let mime = source
                    .strip_prefix("data:")
                    .and_then(|rest| rest.split([';', ',']).next())
                    .filter(|mime| mime.contains('/'))
                    .unwrap_or("application/octet-stream");
                serde_json::json!({"type":"image","mimeType":mime,"data":source})
            } else if source.starts_with("workspace://") || source.starts_with("file://") {
                serde_json::json!({"type":"image","uri":source})
            } else if source.starts_with('/') {
                serde_json::json!({"type":"image","path":source})
            } else {
                serde_json::json!({"type":"image","url":source})
            }
        })
        .collect()
}

fn message_content_blocks(blocks: &Value, include_text: bool, skip_images: bool) -> Value {
    let Some(blocks) = blocks.as_array() else {
        return blocks.clone();
    };
    Value::Array(
        blocks
            .iter()
            .filter(|block| {
                let content = block
                    .get("content")
                    .filter(|value| value.is_object())
                    .unwrap_or(block);
                let kind = content["type"]
                    .as_str()
                    .or(block["type"].as_str())
                    .unwrap_or("");
                (include_text || kind != "text")
                    && (!skip_images || !matches!(kind, "image" | "localImage"))
            })
            .cloned()
            .collect(),
    )
}

fn decode_uri_path(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = (bytes[index + 1] as char).to_digit(16);
            let low = (bytes[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                decoded.push(((high << 4) | low) as u8);
                index += 3;
                continue;
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn cached_resource_bytes(resource: &CachedResource) -> usize {
    resource.mime.len()
        + match &resource.preview {
            CachedResourcePreview::Text {
                text,
                original_base64,
                ..
            } => text.len() + original_base64.as_ref().map_or(0, |encoded| encoded.len()),
            CachedResourcePreview::Binary {
                hex_preview,
                decode_note,
                original_base64,
                ..
            } => {
                hex_preview.len()
                    + decode_note.as_ref().map_or(0, String::len)
                    + original_base64.as_ref().map_or(0, |encoded| encoded.len())
            }
        }
}

fn discard_resource_original(preview: &mut CachedResourcePreview) {
    match preview {
        CachedResourcePreview::Text {
            original_base64, ..
        }
        | CachedResourcePreview::Binary {
            original_base64, ..
        } => *original_base64 = None,
    }
}

fn insert_cached_resource(
    cache: &mut HashMap<String, CachedResource>,
    key: String,
    mut resource: CachedResource,
    max_entries: usize,
    max_bytes: usize,
) {
    cache.remove(&key);
    if cached_resource_bytes(&resource) > max_bytes {
        discard_resource_original(&mut resource.preview);
    }
    let resource_size = cached_resource_bytes(&resource);
    while cache.len() >= max_entries
        || cache
            .values()
            .map(cached_resource_bytes)
            .sum::<usize>()
            .saturating_add(resource_size)
            > max_bytes
    {
        if let Some(evict) = cache.keys().next().cloned() {
            cache.remove(&evict);
        } else {
            break;
        }
    }
    cache.insert(key, resource);
}

fn make_attachment_cache_room(
    images: &mut HashMap<String, CachedImageTexture>,
    audio: &mut HashMap<String, CachedAudio>,
    key: &str,
    incoming_bytes: usize,
) -> bool {
    make_attachment_cache_room_with_limits(
        images,
        audio,
        key,
        incoming_bytes,
        MAX_ATTACHMENT_CACHE_ENTRIES,
        MAX_ATTACHMENT_CACHE_BYTES,
    )
}

fn make_attachment_cache_room_with_limits(
    images: &mut HashMap<String, CachedImageTexture>,
    audio: &mut HashMap<String, CachedAudio>,
    key: &str,
    incoming_bytes: usize,
    max_entries: usize,
    max_bytes: usize,
) -> bool {
    images.remove(key);
    audio.remove(key);
    if incoming_bytes > max_bytes || max_entries == 0 {
        return false;
    }
    while images.len() + audio.len() + 1 > max_entries
        || images
            .values()
            .map(|image| image.decoded_bytes)
            .sum::<usize>()
            .saturating_add(
                audio
                    .values()
                    .map(|clip| clip.bytes.len().saturating_add(clip.mime.len()))
                    .sum::<usize>(),
            )
            .saturating_add(incoming_bytes)
            > max_bytes
    {
        if let Some(oldest) = images.keys().next().cloned() {
            images.remove(&oldest);
        } else if let Some(oldest) = audio.keys().next().cloned() {
            audio.remove(&oldest);
        } else {
            break;
        }
    }
    true
}

fn item_key(identity: &str, item: &Value) -> String {
    format!("{identity}/{}", item["id"].as_str().unwrap_or("anonymous"))
}

fn contains_terminal_content(value: &Value) -> bool {
    match value {
        Value::Array(items) => items.iter().any(contains_terminal_content),
        Value::Object(fields) => {
            (fields.get("type").and_then(Value::as_str) == Some("terminal")
                && fields.get("terminalId").and_then(Value::as_str).is_some())
                || fields.values().any(contains_terminal_content)
        }
        _ => false,
    }
}

fn media_key(identity: &str, attachment_id: &str) -> String {
    format!("{identity}/attachment/{attachment_id}")
}

fn hash_text(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut state = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut state);
    state.finish()
}

#[cfg(test)]
fn cached_blocks(
    cache: &mut HashMap<String, CachedItem>,
    key: &str,
    text: &str,
    last_used: u64,
) -> Vec<MarkdownBlock> {
    let hash = hash_text(text);
    if let Some(cached) = cache.get_mut(key).filter(|cached| cached.hash == hash) {
        cached.last_used = last_used;
        return cached.blocks.clone();
    }
    let parsed = parse_markdown_with_source_ranges(text);
    let blocks = parsed.blocks;
    if cache.len() >= MAX_MARKDOWN_CACHE_ENTRIES {
        if let Some(evict) = cache
            .iter()
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(key, _)| key.clone())
        {
            cache.remove(&evict);
        }
    }
    cache.insert(
        key.to_owned(),
        CachedItem {
            hash,
            last_used,
            blocks: blocks.clone(),
            source_ranges: parsed.source_ranges,
        },
    );
    blocks
}

fn cache_height(heights: &mut HashMap<String, f32>, key: String, height: f32) {
    heights.insert(key, height);
    if heights.len() > MAX_HEIGHT_CACHE_ENTRIES {
        if let Some(evict) = heights.keys().next().cloned() {
            heights.remove(&evict);
        }
    }
}

fn visible_range(
    prefix: &[f32],
    item_count: usize,
    top: f32,
    bottom: f32,
) -> std::ops::Range<usize> {
    let start = prefix
        .partition_point(|offset| *offset < top)
        .saturating_sub(1)
        .min(item_count);
    let mut end = prefix
        .partition_point(|offset| *offset <= bottom)
        .min(item_count);
    if start < item_count && end <= start {
        end = (start + 1).min(item_count);
    }
    start..end.max(start)
}

/// Convert egui's screen-positioned cursor to the content-relative coordinate
/// used by the virtualizer's prefix table.
fn laid_out_content_height(cursor_y: f32, content_origin: f32) -> f32 {
    cursor_y - content_origin
}

/// Distance from the visible viewport top to a row. `row_y` and
/// `content_origin` are screen coordinates; `viewport_top` is relative to the
/// scroll area's content, as returned by egui `show_viewport`.
fn viewport_row_offset(row_y: f32, content_origin: f32, viewport_top: f32) -> f32 {
    row_y - content_origin - viewport_top
}

fn looks_like_diff(text: &str) -> bool {
    text.lines()
        .any(|line| line.starts_with("diff --git ") || line.starts_with("@@ "))
}

fn file_change_has_separate_diffs(item: &Value) -> bool {
    item["toolType"] == "fileChange"
        && item["changes"]
            .as_array()
            .is_some_and(|changes| !changes.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_terminal_tool_content_exposes_read_action_without_duplicate_button() {
        use egui_kittest::{kittest::Queryable, Harness};
        use std::{cell::RefCell, rc::Rc};

        let items = vec![serde_json::json!({
            "id":"tool-42",
            "kind":"tool",
            "title":"Command",
            "terminalId":"terminal-42",
            "content":[{"type":"terminal","terminalId":"terminal-42"}]
        })];
        let actions = Rc::new(RefCell::new(Vec::new()));
        let captured_actions = actions.clone();
        let mut view = ConversationView::default();
        let mut harness = Harness::new_ui(move |ui| {
            captured_actions
                .borrow_mut()
                .extend(view.show(ui, &items, "local/store/session").actions);
        });

        assert!(harness
            .query_by_label("Read command terminal output")
            .is_none());
        harness.get_by_label("Command").click();
        harness.run();
        harness.get_by_label("Read command terminal output").click();
        harness.run();

        assert_eq!(
            actions.borrow().as_slice(),
            &[ContentAction::ReadAcpTerminal {
                terminal_id: "terminal-42".into()
            }]
        );
    }

    #[test]
    fn virtualizer_padding_and_scroll_anchor_stay_content_relative_after_scroll() {
        let total_content_height = 10_000.0;
        let laid_out_rows_end = 680.0;
        let row_distance_from_viewport_top = 92.0;

        for scroll_offset in [0.0, 420.0, 1_800.0, 8_000.0] {
            let viewport_screen_top = 240.0;
            let content_origin = viewport_screen_top - scroll_offset;
            let cursor_y = content_origin + laid_out_rows_end;
            let row_y = viewport_screen_top + row_distance_from_viewport_top;

            let laid_out_height = laid_out_content_height(cursor_y, content_origin);
            let bottom_padding = (total_content_height - laid_out_height).max(0.0);
            let row_offset = viewport_row_offset(row_y, content_origin, scroll_offset);

            assert_eq!(laid_out_height, laid_out_rows_end);
            assert_eq!(bottom_padding, total_content_height - laid_out_rows_end);
            assert_eq!(row_offset, row_distance_from_viewport_top);
            assert_eq!(laid_out_height + bottom_padding, total_content_height);
        }
    }

    #[test]
    fn codex_image_projection_routes_data_files_and_external_urls_through_content_blocks() {
        let blocks = codex_image_blocks(&serde_json::json!([
            "data:image/png;base64,aGVsbG8=",
            "file:///workspace/img%20one.png",
            "/workspace/two.png",
            "https://example.test/three.png"
        ]));
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0]["type"], "image");
        assert_eq!(blocks[0]["mimeType"], "image/png");
        assert_eq!(blocks[0]["data"], "data:image/png;base64,aGVsbG8=");
        assert_eq!(blocks[1]["uri"], "file:///workspace/img%20one.png");
        assert_eq!(blocks[2]["path"], "/workspace/two.png");
        assert_eq!(blocks[3]["url"], "https://example.test/three.png");
    }

    #[test]
    fn canonical_message_fields_avoid_duplicate_text_and_images_but_keep_other_blocks() {
        let raw = serde_json::json!([
            {"type":"text","text":"Hello"},
            {"type":"image","path":"/tmp/image.png"},
            {"type":"localImage","path":"/tmp/local.png"},
            {"type":"resource_link","uri":"workspace://notes.txt"}
        ]);
        let filtered = message_content_blocks(&raw, false, true);
        assert_eq!(
            filtered,
            serde_json::json!([{"type":"resource_link","uri":"workspace://notes.txt"}])
        );
        let no_projection = message_content_blocks(&raw, true, false);
        assert_eq!(no_projection, raw);
    }

    #[test]
    fn file_change_renders_per_file_diffs_without_repeating_combined_output() {
        assert!(file_change_has_separate_diffs(&serde_json::json!({
            "toolType": "fileChange",
            "output": "@@ -1 +1 @@\n-old\n+new",
            "changes": [{"path": "a.rs", "diff": "@@ -1 +1 @@\n-old\n+new"}]
        })));
        assert!(!file_change_has_separate_diffs(&serde_json::json!({
            "toolType": "commandExecution",
            "output": "@@ -1 +1 @@\n-old\n+new",
            "changes": [{"path": "a.rs", "diff": "@@ -1 +1 @@\n-old\n+new"}]
        })));
        assert!(!file_change_has_separate_diffs(&serde_json::json!({
            "toolType": "fileChange",
            "changes": []
        })));
    }

    #[test]
    fn attachment_cache_enforces_aggregate_entry_and_byte_bounds() {
        let mut images = HashMap::new();
        let mut audio = HashMap::new();
        for index in 0..3 {
            let key = format!("conversation/clip-{index}");
            assert!(make_attachment_cache_room_with_limits(
                &mut images,
                &mut audio,
                &key,
                4,
                3,
                10
            ));
            audio.insert(
                key,
                CachedAudio {
                    mime: "a".into(),
                    bytes: vec![0; 3],
                },
            );
        }
        assert_eq!(audio.len(), 2, "entry cap should evict an existing item");
        assert!(make_attachment_cache_room_with_limits(
            &mut images,
            &mut audio,
            "conversation/large",
            9,
            3,
            10
        ));
        audio.insert(
            "conversation/large".into(),
            CachedAudio {
                mime: "a".into(),
                bytes: vec![0; 8],
            },
        );
        let total_bytes: usize = audio
            .values()
            .map(|clip| clip.bytes.len() + clip.mime.len())
            .sum();
        assert!(
            total_bytes <= 10,
            "aggregate media bytes must remain bounded"
        );
        assert!(!make_attachment_cache_room_with_limits(
            &mut images,
            &mut audio,
            "conversation/too-large",
            11,
            3,
            10
        ));
        assert!(audio.len() <= 3);
    }

    #[test]
    fn versioned_diff_worker_parses_once_per_revision_and_refreshes_changed_content() {
        let context = Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 500.0))),
            ..Default::default()
        };
        let mut view = DiffView::default();
        let first = "@@ -1 +1 @@\n-old\n+new";
        let second = "@@ -1 +1 @@\n-before\n+after";

        let drive = |view: &mut DiffView, value: &str, revision| {
            let mut output = context.run_ui(input.clone(), |ui| {
                view.show_text_versioned(
                    ui,
                    "workspace/file.rs",
                    value,
                    revision,
                    DiffMode::Unified,
                );
            });
            output.textures_delta.clear();
        };
        drive(&mut view, &first, 1);
        for _ in 0..200 {
            drive(&mut view, &first, 1);
            if view
                .cache
                .get("workspace/file.rs")
                .is_some_and(|model| model.revision == 1)
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(view.completed_parses, 1);
        for _ in 0..20 {
            drive(&mut view, &first, 1);
        }
        assert_eq!(
            view.completed_parses, 1,
            "unchanged revision must not reparse"
        );

        drive(&mut view, &second, 2);
        for _ in 0..200 {
            drive(&mut view, &second, 2);
            if view
                .cache
                .get("workspace/file.rs")
                .is_some_and(|model| model.revision == 2)
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(view.completed_parses, 2);
        let model = &view.cache["workspace/file.rs"];
        assert_eq!(model.revision, 2);
        assert!(model.rows.iter().any(|row| row.text == "after"));
        assert!(!model.rows.iter().any(|row| row.text == "new"));
    }

    #[test]
    fn cache_invalidates_when_streamed_text_changes_and_keeps_duplicate_order() {
        let mut cache = HashMap::new();
        let id = "backend/store/session";
        let ka = format!("{id}/same-a");
        let kb = format!("{id}/same-b");
        let a = cached_blocks(&mut cache, &ka, "repeat 🐉", 1);
        let b = cached_blocks(&mut cache, &kb, "repeat 🐉", 2);
        assert_ne!(ka, kb);
        assert_eq!(a, b);
        let previous_hash = cache[&ka].hash;
        let changed = cached_blocks(&mut cache, &ka, "repeat 🐉 again", 3);
        assert_ne!(cache[&ka].hash, previous_hash);
        assert!(changed.iter().any(|block| matches!(block, MarkdownBlock::Paragraph(spans) if spans.iter().any(|span| span.text.contains("again")))));
        assert_eq!(cache[&kb].hash, previous_hash);
    }

    #[test]
    fn markdown_cache_evicts_cold_rows_and_keeps_recent_parses() {
        let mut cache = HashMap::new();
        for index in 0..MAX_MARKDOWN_CACHE_ENTRIES {
            cached_blocks(
                &mut cache,
                &format!("row-{index}"),
                "**same** 🐉",
                index as u64,
            );
        }
        cached_blocks(&mut cache, "row-0", "**same** 🐉", 50_000);
        cached_blocks(&mut cache, "new-visible-row", "**same** 🐉", 50_001);
        assert_eq!(cache.len(), MAX_MARKDOWN_CACHE_ENTRIES);
        assert!(cache.contains_key("row-0"));
        assert!(cache.contains_key("new-visible-row"));
        assert!(!cache.contains_key("row-1"));
    }

    #[test]
    fn markdown_selection_text_preserves_exact_unicode_across_format_spans() {
        let spans = match parse_markdown("Welsh **cariad 🐉**, *cân* and `naïve`.")
            .into_iter()
            .next()
            .unwrap()
        {
            MarkdownBlock::Paragraph(spans) => spans,
            other => panic!("expected paragraph, got {other:?}"),
        };
        assert_eq!(plain_text(&spans), "Welsh cariad 🐉, cân and naïve.");
        assert!(spans
            .iter()
            .any(|span| span.strong && span.text.contains("cariad")));
        assert!(spans.iter().any(|span| span.emphasis && span.text == "cân"));
        assert!(spans.iter().any(|span| span.code && span.text == "naïve"));
        let job = spans_layout_job(&egui::Style::default(), &spans, None);
        assert_eq!(job.text, "Welsh cariad 🐉, cân and naïve.");
        assert!(
            job.sections.len() > 1,
            "formatted runs must share one layout job"
        );
    }

    #[test]
    fn viewport_range_for_ten_thousand_rows_is_bounded() {
        let prefix: Vec<f32> = (0..=10_000).map(|index| index as f32 * 66.0).collect();
        let range = visible_range(&prefix, 10_000, 420_000.0, 420_800.0);
        assert!(range.end - range.start < 20);
        assert!(range.start < 10_000 && range.end <= 10_000);
    }

    #[test]
    fn links_resolve_to_explicit_host_actions() {
        assert_eq!(
            link_action("workspace://src/lib.rs"),
            ContentAction::OpenFile {
                path: "src/lib.rs".into()
            }
        );
        assert_eq!(
            link_action("file:///tmp/notes%20here.md"),
            ContentAction::OpenFile {
                path: "/tmp/notes%20here.md".into()
            }
        );
        assert_eq!(
            link_action("https://example.test"),
            ContentAction::OpenLink {
                url: "https://example.test".into()
            }
        );
    }

    #[test]
    fn attachments_route_inline_workspace_and_store_sources_without_guessing() {
        assert_eq!(
            attachment_action(
                &serde_json::json!({"type":"image","mimeType":"image/png","data":"data:image/png;base64,cG5n"}),
                "image-1"
            ),
            Some(ContentAction::LoadInlineAttachment {
                id: "image-1".into(),
                mime: "image/png".into(),
                data: "data:image/png;base64,cG5n".into()
            })
        );
        assert_eq!(
            attachment_action(
                &serde_json::json!({"type":"image","uri":"file:///work/repo/images/c%C3%A2n.png","mimeType":"image/png"}),
                "file:///work/repo/images/c%C3%A2n.png"
            ),
            Some(ContentAction::LoadWorkspaceAttachment {
                id: "file:///work/repo/images/c%C3%A2n.png".into(),
                path: "/work/repo/images/cân.png".into()
            })
        );
        assert_eq!(
            attachment_action(
                &serde_json::json!({"type":"resource","uri":"workspace://assets/my%20file.pdf","mimeType":"application/pdf"}),
                "document"
            ),
            Some(ContentAction::LoadWorkspaceAttachment {
                id: "document".into(),
                path: "assets/my file.pdf".into()
            })
        );
        assert_eq!(
            attachment_action(
                &serde_json::json!({"type":"audio","attachmentId":"acp-attachment-4","mimeType":"audio/wav"}),
                "acp-attachment-4"
            ),
            Some(ContentAction::LoadAttachment {
                id: "acp-attachment-4".into()
            })
        );
        assert!(attachment_action(
            &serde_json::json!({"type":"resource","uri":"https://example.test/a.pdf"}),
            ""
        )
        .is_none());
    }

    #[test]
    fn loaded_binary_resources_keep_mime_length_preview_and_failure_note() {
        let mut view = ConversationView::default();
        view.cache_resource_binary(
            "backend/store/session",
            "pdf-1",
            "application/pdf",
            8,
            "25 50 44 46".into(),
            None,
        );
        let resource = &view.resource_cache["backend/store/session/attachment/pdf-1"];
        assert_eq!(resource.mime, "application/pdf");
        assert!(
            matches!(&resource.preview, CachedResourcePreview::Binary { byte_len: 8, hex_preview, .. } if hex_preview == "25 50 44 46")
        );

        view.cache_resource_binary(
            "backend/store/session",
            "broken-image",
            "image/png",
            9,
            "6e 6f 74".into(),
            Some("Image decode failed".into()),
        );
        assert!(
            matches!(&view.resource_cache["backend/store/session/attachment/broken-image"].preview, CachedResourcePreview::Binary { decode_note: Some(note), .. } if note == "Image decode failed")
        );
    }

    #[test]
    fn original_resource_copy_data_is_retained_and_cache_is_bounded_by_total_bytes() {
        let mut view = ConversationView::default();
        view.cache_resource_binary_original(
            "backend/store/session",
            "full-copy",
            "application/pdf",
            240,
            "00 01 02".into(),
            None,
            "AAECAw==".into(),
        );
        assert!(matches!(
            &view.resource_cache["backend/store/session/attachment/full-copy"].preview,
            CachedResourcePreview::Binary { original_base64: Some(encoded), .. } if encoded.as_ref() == "AAECAw=="
        ));

        let mut cache = HashMap::new();
        let resource = |encoded: &str| CachedResource {
            mime: "x".into(),
            preview: CachedResourcePreview::Binary {
                byte_len: 18,
                hex_preview: "00".into(),
                decode_note: None,
                original_base64: Some(Arc::<str>::from(encoded)),
            },
        };
        insert_cached_resource(&mut cache, "old".into(), resource(&"a".repeat(20)), 8, 40);
        insert_cached_resource(&mut cache, "new".into(), resource(&"b".repeat(20)), 8, 40);
        assert_eq!(cache.len(), 1);
        assert!(cache.contains_key("new"));
        assert!(cache.values().map(cached_resource_bytes).sum::<usize>() <= 40);

        insert_cached_resource(
            &mut cache,
            "oversize".into(),
            resource(&"c".repeat(100)),
            8,
            40,
        );
        assert!(matches!(
            &cache["oversize"].preview,
            CachedResourcePreview::Binary {
                original_base64: None,
                ..
            }
        ));
        assert!(cache.values().map(cached_resource_bytes).sum::<usize>() <= 40);
    }

    #[test]
    fn cached_media_and_scroll_anchors_are_scoped_to_the_conversation() {
        let mut view = ConversationView::default();
        view.cache_audio("remote/store/a", "clip", "audio/ogg", vec![1, 2, 3]);
        view.cache_audio("local/store/a", "clip", "audio/wav", vec![4]);
        assert_eq!(
            view.audio_cache["remote/store/a/attachment/clip"].mime,
            "audio/ogg"
        );
        assert_eq!(
            view.audio_cache["remote/store/a/attachment/clip"].bytes,
            vec![1, 2, 3]
        );
        assert_eq!(
            view.audio_cache["local/store/a/attachment/clip"].bytes,
            vec![4]
        );

        view.set_scroll_anchor("message-17", 12.5);
        assert_eq!(view.scroll_anchor, Some(("message-17".into(), 12.5)));
        view.clear_scroll_anchor();
        assert_eq!(view.scroll_anchor, None);
    }

    #[test]
    fn rendered_conversation_reports_a_restorable_visible_row_anchor() {
        let ctx = egui::Context::default();
        let mut view = ConversationView::default();
        let items = vec![serde_json::json!({
            "id": "turn-7",
            "kind": "message",
            "role": "assistant",
            "text": "Cân 🐉"
        })];
        let mut response = None;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::new(800.0, 500.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            response = Some(view.show(ui, &items, "backend/store/session"));
        });
        output.textures_delta.clear();
        let anchor = response.unwrap().scroll_anchor.unwrap();
        assert_eq!(anchor.item_id, "turn-7");
        assert!(anchor.offset.is_finite());

        view.set_scroll_anchor(anchor.item_id, anchor.offset);
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::Vec2::new(800.0, 500.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            let _ = view.show(ui, &items, "backend/store/session");
        });
        output.textures_delta.clear();
        assert_eq!(view.scroll_anchor, None);
    }

    #[test]
    fn markdown_and_code_highlighting_prepare_off_draw_and_wake_the_ui() {
        let ctx = egui::Context::default();
        let mut view = ConversationView::default();
        let identity = "backend/store/worker-test";
        let item = serde_json::json!({
            "id": "message-1",
            "kind": "message",
            "role": "assistant",
            "text": "Welsh **cariad 🐉**.\n\n```rust\nfn main() {}\n```"
        });
        let items = vec![item];
        let screen_rect =
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::new(800.0, 500.0));
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                ..Default::default()
            },
            |ui| {
                let _ = view.show(ui, &items, identity);
            },
        );
        output.textures_delta.clear();
        assert!(
            view.cache.is_empty(),
            "first draw must only enqueue parsing"
        );
        assert_eq!(view.pending_markdown.len(), 1);

        for _ in 0..200 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen_rect),
                    ..Default::default()
                },
                |ui| {
                    let _ = view.show(ui, &items, identity);
                },
            );
            output.textures_delta.clear();
            if !view.cache.is_empty() && !view.highlighted_code.is_empty() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(view
            .cache
            .values()
            .any(|entry| entry.blocks.iter().any(|block| {
                matches!(block, MarkdownBlock::Code { text, .. } if text.contains("fn main"))
            })));
        assert!(!view.highlighted_code.is_empty());
        for _ in 0..3 {
            let mut output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(screen_rect),
                    ..Default::default()
                },
                |ui| {
                    let _ = view.show(ui, &items, identity);
                },
            );
            output.textures_delta.clear();
        }
        assert!(view.pending_markdown.is_empty());
        assert!(view.pending_highlights.is_empty());
    }

    #[test]
    fn height_estimate_cache_stays_bounded() {
        let mut heights = HashMap::new();
        for index in 0..(MAX_HEIGHT_CACHE_ENTRIES + 1) {
            cache_height(&mut heights, format!("row-{index}"), 44.0);
        }
        assert_eq!(heights.len(), MAX_HEIGHT_CACHE_ENTRIES);
    }
}

fn plain_text(spans: &[markdown::Span]) -> String {
    spans.iter().map(|span| span.text.as_str()).collect()
}
