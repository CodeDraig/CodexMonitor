use crate::types::AppSettings;
use egui::{Color32, Context, FontData, FontDefinitions, FontFamily, Id, TextStyle};
use fontique::{
    Collection, CollectionOptions, GenericFamily, QueryFamily, QueryStatus, SourceCache,
};
use std::{
    sync::{Arc, Mutex},
    thread,
};

pub(super) const ACCENT: Color32 = Color32::from_rgb(119, 166, 226);
pub(super) const MUTED: Color32 = Color32::from_rgb(145, 156, 172);
pub(super) const ERROR: Color32 = Color32::from_rgb(240, 142, 142);

const UI_FONT_DATA_NAME: &str = "codex-monitor-ui-system-font";
const CODE_FONT_DATA_NAME: &str = "codex-monitor-code-system-font";

#[derive(Default)]
struct FontRequestState {
    generation: u64,
    ui_family: Option<String>,
    code_family: Option<String>,
}

static FONT_REQUEST_STATE: Mutex<FontRequestState> = Mutex::new(FontRequestState {
    generation: 0,
    ui_family: None,
    code_family: None,
});

#[derive(Clone, Debug, PartialEq, Eq)]
enum CssFamily {
    Named(String),
    Generic(GenericFamily),
}

fn family_list(css_families: &str) -> Vec<CssFamily> {
    let mut result = Vec::new();
    for value in css_families.split(',') {
        let family = value
            .trim()
            .trim_matches(|character| character == '\'' || character == '"')
            .trim();
        if family.is_empty() {
            continue;
        }
        let parsed = parse_css_family(family);
        if !result.iter().any(|existing| same_family(existing, &parsed)) {
            result.push(parsed);
        }
    }
    result
}

fn parse_css_family(name: &str) -> CssFamily {
    if name.eq_ignore_ascii_case("-apple-system") || name.eq_ignore_ascii_case("BlinkMacSystemFont")
    {
        return CssFamily::Generic(GenericFamily::SystemUi);
    }
    if let Some(generic) = GenericFamily::parse(name) {
        return CssFamily::Generic(generic);
    }
    CssFamily::Named(name.to_owned())
}

fn same_family(left: &CssFamily, right: &CssFamily) -> bool {
    match (left, right) {
        (CssFamily::Named(left), CssFamily::Named(right)) => left.eq_ignore_ascii_case(right),
        (CssFamily::Generic(left), CssFamily::Generic(right)) => left == right,
        _ => false,
    }
}

fn family_stack_label(families: &[CssFamily]) -> String {
    families
        .iter()
        .map(|family| match family {
            CssFamily::Named(name) => name.clone(),
            CssFamily::Generic(generic) => generic.to_string(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn has_generic_family(families: &[CssFamily]) -> bool {
    families
        .iter()
        .any(|family| matches!(family, CssFamily::Generic(_)))
}

fn should_report_missing_font(families: &[CssFamily], resolved: bool) -> bool {
    !resolved && !families.is_empty() && !has_generic_family(families)
}

fn query_family(family: &CssFamily) -> QueryFamily<'_> {
    match family {
        CssFamily::Named(name) => QueryFamily::Named(name),
        CssFamily::Generic(generic) => QueryFamily::Generic(*generic),
    }
}

fn apply_code_font_size(style: &mut egui::Style, size: u8) {
    if let Some(font_id) = style.text_styles.get_mut(&TextStyle::Monospace) {
        font_id.size = f32::from(size.clamp(8, 32));
    }
}

fn apply_palette(style: &mut egui::Style, theme_name: &str) {
    style.visuals = if theme_name == "light" {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };

    match theme_name {
        "light" => {
            style.visuals.selection.bg_fill = Color32::from_rgb(216, 231, 248);
        }
        "dim" => {
            style.visuals.panel_fill = Color32::from_rgb(34, 39, 46);
            style.visuals.window_fill = Color32::from_rgb(45, 51, 59);
            style.visuals.extreme_bg_color = Color32::from_rgb(27, 32, 38);
            style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(51, 58, 67);
            style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(64, 73, 84);
            style.visuals.selection.bg_fill = Color32::from_rgb(48, 75, 109);
        }
        _ => {
            style.visuals.panel_fill = Color32::from_rgb(20, 23, 28);
            style.visuals.window_fill = Color32::from_rgb(34, 40, 49);
            style.visuals.extreme_bg_color = Color32::from_rgb(15, 18, 23);
            style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(37, 44, 54);
            style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(48, 58, 71);
            style.visuals.selection.bg_fill = Color32::from_rgb(35, 59, 89);
        }
    }
    // egui uses the selection stroke as selected-row text as well as its
    // outline. Keep that text readable against the selected surface.
    style.visuals.selection.stroke = egui::Stroke::new(
        1.0,
        if theme_name == "light" {
            Color32::from_rgb(35, 62, 94)
        } else {
            Color32::from_rgb(220, 233, 250)
        },
    );
}

fn configure_style(mut style: egui::Style, palette: &str, code_font_size: u8) -> egui::Style {
    apply_palette(&mut style, palette);
    style.spacing.item_spacing = egui::vec2(8.0, 7.0);
    style.spacing.button_padding = egui::vec2(9.0, 5.0);
    apply_code_font_size(&mut style, code_font_size);
    style
}

pub(super) fn apply(ctx: &Context, settings: &AppSettings) {
    ctx.set_theme(if settings.theme == "system" {
        egui::ThemePreference::System
    } else {
        egui::ThemePreference::from(if settings.theme == "light" {
            egui::Theme::Light
        } else {
            egui::Theme::Dark
        })
    });
    ctx.set_zoom_factor((settings.ui_scale as f32).clamp(0.5, 3.0));

    let themes: &[(egui::Theme, &str)] = if settings.theme == "system" {
        &[(egui::Theme::Light, "light"), (egui::Theme::Dark, "dark")]
    } else if settings.theme == "light" {
        &[(egui::Theme::Light, "light")]
    } else if settings.theme == "dim" {
        &[(egui::Theme::Dark, "dim")]
    } else {
        &[(egui::Theme::Dark, "dark")]
    };
    for (theme, palette) in themes {
        let style = configure_style(
            (*ctx.style_of(*theme)).clone(),
            palette,
            settings.code_font_size,
        );
        ctx.set_style_of(*theme, style);
    }

    schedule_font_load(
        ctx,
        settings.ui_font_family.clone(),
        settings.code_font_family.clone(),
    );
}

fn schedule_font_load(ctx: &Context, ui_family: String, code_family: String) {
    let generation = {
        let mut state = FONT_REQUEST_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.ui_family.as_deref() == Some(ui_family.as_str())
            && state.code_family.as_deref() == Some(code_family.as_str())
        {
            return;
        }
        state.generation = state.generation.wrapping_add(1);
        state.ui_family = Some(ui_family.clone());
        state.code_family = Some(code_family.clone());
        state.generation
    };

    let worker_ctx = ctx.clone();
    let warning_ctx = ctx.clone();
    let worker = thread::Builder::new()
        .name("codex-monitor-font-loader".into())
        .spawn(move || {
            let (definitions, warning) = build_font_definitions(&ui_family, &code_family);
            // Keep the generation lock through the egui update. A newer
            // settings request cannot be accepted between this check and the
            // write, so an older worker cannot land after a newer request.
            let state = FONT_REQUEST_STATE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.generation != generation {
                return;
            }
            worker_ctx.set_fonts(definitions);
            worker_ctx.data_mut(|data| {
                if let Some(warning) = warning {
                    data.insert_temp(font_warning_id(), warning);
                } else {
                    data.remove_temp::<String>(font_warning_id());
                }
            });
            drop(state);
            worker_ctx.request_repaint();
        });

    if let Err(error) = worker {
        let warning = format!(
            "Could not start the font loader ({error}); keeping egui's embedded font fallback."
        );
        eprintln!("Codex Monitor: {warning}");
        let state = FONT_REQUEST_STATE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.generation == generation {
            warning_ctx.data_mut(|data| data.insert_temp(font_warning_id(), warning));
        }
    }
}

fn build_font_definitions(ui_family: &str, code_family: &str) -> (FontDefinitions, Option<String>) {
    let mut definitions = FontDefinitions::default();
    let ui_families = family_list(ui_family);
    let code_families = family_list(code_family);
    let mut collection = Collection::new(CollectionOptions::default());
    let mut source_cache = SourceCache::default();
    let ui_font = resolve_system_font(&mut collection, &mut source_cache, &ui_families);
    let code_font = resolve_system_font(&mut collection, &mut source_cache, &code_families);
    let mut warnings = Vec::new();

    if let Some(font) = ui_font {
        insert_font(
            &mut definitions,
            UI_FONT_DATA_NAME,
            font,
            FontFamily::Proportional,
        );
    } else if should_report_missing_font(&ui_families, false) {
        warnings.push(format!(
            "UI font family '{}' was not found; using egui's embedded proportional font.",
            family_stack_label(&ui_families)
        ));
    }

    if let Some(font) = code_font {
        insert_font(
            &mut definitions,
            CODE_FONT_DATA_NAME,
            font,
            FontFamily::Monospace,
        );
    } else if should_report_missing_font(&code_families, false) {
        warnings.push(format!(
            "Code font family '{}' was not found; using egui's embedded monospace font.",
            family_stack_label(&code_families)
        ));
    }

    let warning = (!warnings.is_empty()).then(|| warnings.join(" "));
    if let Some(warning) = &warning {
        eprintln!("Codex Monitor: {warning}");
    }
    (definitions, warning)
}

struct LoadedFont {
    bytes: Vec<u8>,
    index: u32,
}

fn resolve_system_font(
    collection: &mut Collection,
    source_cache: &mut SourceCache,
    families: &[CssFamily],
) -> Option<LoadedFont> {
    if families.is_empty() {
        return None;
    }
    let mut query = collection.query(source_cache);
    query.set_families(families.iter().map(query_family));
    let mut result = None;
    query.matches_with(|font| {
        result = Some(LoadedFont {
            bytes: font.blob.as_ref().to_vec(),
            index: font.index,
        });
        QueryStatus::Stop
    });
    result
}

fn insert_font(
    definitions: &mut FontDefinitions,
    name: &str,
    loaded: LoadedFont,
    family: FontFamily,
) {
    let font_blob: egui::epaint::text::Blob = Arc::new(loaded.bytes);
    definitions.font_data.insert(
        name.to_owned(),
        Arc::new(FontData::from_blob(font_blob, loaded.index)),
    );
    let fonts = definitions.families.entry(family).or_default();
    fonts.retain(|font_name| font_name != name);
    fonts.insert(0, name.to_owned());
}

fn font_warning_id() -> Id {
    Id::new("native-theme-font-fallback-notice")
}

/// Consume a notice when either requested system font fell back to egui's
/// embedded face. Native UI can display this rather than implying the choice
/// was honored.
pub(super) fn take_font_fallback_notice(ctx: &Context) -> Option<String> {
    ctx.data_mut(|data| data.remove_temp(font_warning_id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_font_families_preserve_named_and_generic_order_and_deduplicate() {
        assert_eq!(
            family_list("system-ui, \"A Font\", 'A Font', Roboto, sans-serif"),
            vec![
                CssFamily::Generic(GenericFamily::SystemUi),
                CssFamily::Named("A Font".to_owned()),
                CssFamily::Named("Roboto".to_owned()),
                CssFamily::Generic(GenericFamily::SansSerif),
            ]
        );
        assert_eq!(
            family_list("ui-monospace, monospace, UI-MONOSPACE"),
            vec![
                CssFamily::Generic(GenericFamily::UiMonospace),
                CssFamily::Generic(GenericFamily::Monospace),
            ]
        );
    }

    #[test]
    fn generic_css_families_map_to_fontique_queries_in_stack_order() {
        let parsed = family_list("system-ui, \"Segoe UI\", -apple-system, monospace");
        let mapped: Vec<_> = parsed.iter().map(query_family).collect();
        assert!(matches!(
            mapped[0],
            QueryFamily::Generic(GenericFamily::SystemUi)
        ));
        assert!(matches!(mapped[1], QueryFamily::Named("Segoe UI")));
        assert!(matches!(
            mapped[2],
            QueryFamily::Generic(GenericFamily::Monospace)
        ));
    }

    #[test]
    fn default_css_stacks_are_valid_and_do_not_raise_unknown_family_warning() {
        let ui = family_list(
            "system-ui, -apple-system, BlinkMacSystemFont, \"Segoe UI\", Roboto, \"Helvetica Neue\", Arial, sans-serif",
        );
        let code = family_list(
            "ui-monospace, \"Cascadia Mono\", \"Segoe UI Mono\", Menlo, Monaco, Consolas, \"Liberation Mono\", \"Courier New\", monospace",
        );
        assert!(matches!(
            ui.first(),
            Some(CssFamily::Generic(GenericFamily::SystemUi))
        ));
        assert!(matches!(
            code.first(),
            Some(CssFamily::Generic(GenericFamily::UiMonospace))
        ));
        assert!(!should_report_missing_font(&ui, false));
        assert!(!should_report_missing_font(&code, false));
    }

    #[test]
    fn unknown_named_only_family_still_reports_embedded_fallback() {
        let missing = family_list("No Such Font");
        assert!(should_report_missing_font(&missing, false));
        assert!(!should_report_missing_font(&missing, true));
    }

    #[test]
    fn code_font_size_is_applied_and_clamped() {
        let mut style = egui::Style::default();
        apply_code_font_size(&mut style, 21);
        assert_eq!(style.text_styles[&TextStyle::Monospace].size, 21.0);
        apply_code_font_size(&mut style, 1);
        assert_eq!(style.text_styles[&TextStyle::Monospace].size, 8.0);
    }

    #[test]
    fn system_preference_receives_code_size_in_both_theme_variants() {
        let mut light = configure_style(egui::Style::default(), "light", 19);
        let mut dark = configure_style(egui::Style::default(), "dark", 19);
        assert_eq!(light.text_styles[&TextStyle::Monospace].size, 19.0);
        assert_eq!(dark.text_styles[&TextStyle::Monospace].size, 19.0);
        assert_ne!(light.visuals.panel_fill, dark.visuals.panel_fill);
    }

    #[test]
    fn dim_palette_is_distinct_from_dark_and_light() {
        let mut dark = egui::Style::default();
        apply_palette(&mut dark, "dark");
        let mut dim = egui::Style::default();
        apply_palette(&mut dim, "dim");
        let mut light = egui::Style::default();
        apply_palette(&mut light, "light");
        assert_ne!(dark.visuals.panel_fill, dim.visuals.panel_fill);
        assert_ne!(dim.visuals.panel_fill, light.visuals.panel_fill);
        assert_ne!(dark.visuals.window_fill, dim.visuals.window_fill);
    }
}
