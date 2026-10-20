use super::{mutate_prompts, read_prompts, PanelEffect, WorkbenchRequestKey};
use egui::{self, Ui};
use serde_json::{json, Value};

#[derive(Debug, Default)]
pub(crate) struct PromptsPanel {
    workspace: Option<String>,
    stale: bool,
    prompts: Vec<Value>,
    selected: Option<usize>,
    name: String,
    description: String,
    argument_hint: String,
    content: String,
    scope: String,
    error: Option<String>,
    status: Option<String>,
    dirty: bool,
    confirm_delete: bool,
}
impl PromptsPanel {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
    pub(crate) fn mark_stale(&mut self) {
        self.stale = true;
    }
    pub(crate) fn show(
        &mut self,
        ui: &mut Ui,
        effects: &mut Vec<PanelEffect>,
        workspace: Option<&str>,
    ) {
        if self.workspace.as_deref() != workspace {
            self.workspace = workspace.map(str::to_owned);
            self.prompts.clear();
            self.selected = None;
            self.stale = true;
            self.dirty = false;
        }
        ui.horizontal(|ui| {
            ui.heading("Prompts");
            if ui.button("Refresh").clicked() {
                self.stale = true;
            }
            if ui.button("New prompt").clicked() {
                self.selected = None;
                self.name.clear();
                self.description.clear();
                self.argument_hint.clear();
                self.content.clear();
                self.scope = "workspace".into();
                self.dirty = false;
            }
        });
        if self.stale {
            if let Some(w) = workspace {
                effects.push(PanelEffect::Request {
                    key: WorkbenchRequestKey::PromptsList,
                    request: read_prompts("prompts_list", json!({"workspaceId":w})),
                });
                self.stale = false;
            }
        }
        if let Some(err) = &self.error {
            ui.colored_label(egui::Color32::LIGHT_RED, err);
        }
        if let Some(s) = &self.status {
            ui.label(s);
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(self.selected.is_some(), egui::Button::new("Use prompt"))
                .clicked()
            {
                effects.push(PanelEffect::InsertPrompt {
                    prompt: self.editor_payload(),
                });
            }
            if self.selected.is_none() {
                ui.weak("Choose a saved prompt first.");
            }
        });
        ui.columns(2,|cols|{egui::ScrollArea::vertical().show(&mut cols[0],|ui|{for (index,p) in self.prompts.iter().enumerate(){if ui.selectable_label(self.selected==Some(index),format!("{}{}",p["name"].as_str().unwrap_or("(unnamed)"),p["scope"].as_str().map(|s|format!(" · {s}")).unwrap_or_default())).clicked(){self.selected=Some(index);self.name=p["name"].as_str().unwrap_or_default().into();self.description=p["description"].as_str().unwrap_or_default().into();self.argument_hint=p["argumentHint"].as_str().unwrap_or_default().into();self.content=p["content"].as_str().unwrap_or_default().into();self.scope=p["scope"].as_str().unwrap_or("workspace").into();self.dirty=false;}}});cols[1].vertical(|ui|{ui.label("Name");if ui.text_edit_singleline(&mut self.name).changed(){self.dirty=true;}ui.label("Description");if ui.text_edit_singleline(&mut self.description).changed(){self.dirty=true;}ui.label("Argument hint");if ui.text_edit_singleline(&mut self.argument_hint).changed(){self.dirty=true;}if self.selected.is_none(){egui::ComboBox::from_id_salt("prompt-scope").selected_text(&self.scope).show_ui(ui,|ui|{ui.selectable_value(&mut self.scope,"workspace".into(),"Workspace");ui.selectable_value(&mut self.scope,"global".into(),"Global");});}ui.label("Prompt content");if ui.add(egui::TextEdit::multiline(&mut self.content).code_editor().desired_rows(20)).changed(){self.dirty=true;}
   if ui.add_enabled(self.dirty&&!self.name.trim().is_empty(),egui::Button::new("Save prompt")).clicked(){let w=workspace.unwrap_or_default();let params=if let Some(index)=self.selected{json!({"workspaceId":w,"path":self.prompts[index]["path"],"name":self.name,"description":nonempty(&self.description),"argumentHint":nonempty(&self.argument_hint),"content":self.content})}else{json!({"workspaceId":w,"scope":self.scope,"name":self.name,"description":nonempty(&self.description),"argumentHint":nonempty(&self.argument_hint),"content":self.content})};effects.push(PanelEffect::Request{key:WorkbenchRequestKey::PromptWrite,request:mutate_prompts(if self.selected.is_some(){"prompts_update"}else{"prompts_create"},params)});}
   if let Some(index)=self.selected {let path=self.prompts[index]["path"].as_str().unwrap_or_default().to_owned();let next_scope=if self.scope=="global"{"workspace"}else{"global"};if ui.button(format!("Move to {next_scope}")).clicked(){effects.push(PanelEffect::Request{key:WorkbenchRequestKey::PromptMove(path.clone()),request:mutate_prompts("prompts_move",json!({"workspaceId":workspace.unwrap_or_default(),"path":path,"scope":next_scope}))});}if !self.confirm_delete{if ui.button("Delete…").clicked(){self.confirm_delete=true;}}else{ui.horizontal(|ui|{ui.colored_label(egui::Color32::YELLOW,"Delete this prompt?");if ui.button("Confirm delete").clicked(){effects.push(PanelEffect::Request{key:WorkbenchRequestKey::PromptDelete(path.clone()),request:mutate_prompts("prompts_delete",json!({"workspaceId":workspace.unwrap_or_default(),"path":path}))});self.confirm_delete=false;}if ui.button("Cancel").clicked(){self.confirm_delete=false;}});}}});});
    }
    pub(crate) fn apply(&mut self, key: &WorkbenchRequestKey, value: Value) {
        match key {
            WorkbenchRequestKey::PromptsList => {
                self.prompts = value.as_array().cloned().unwrap_or_default();
            }
            WorkbenchRequestKey::PromptWrite => {
                if let Some(v) = self.selected.and_then(|i| self.prompts.get_mut(i)) {
                    *v = value.clone();
                } else {
                    self.prompts.push(value);
                }
                self.dirty = false;
                self.status = Some("Prompt saved".into());
                self.selected = self.prompts.len().checked_sub(1);
                self.stale = false;
            }
            WorkbenchRequestKey::PromptDelete(path) => {
                self.prompts.retain(|p| p["path"].as_str() != Some(path));
                self.selected = None;
                self.status = Some("Prompt deleted".into());
            }
            WorkbenchRequestKey::PromptMove(_) => {
                self.stale = true;
            }
            _ => {}
        }
    }

    fn editor_payload(&self) -> Value {
        json!({
            "name": self.name,
            "content": self.content,
            "argumentHint": nonempty(&self.argument_hint),
        })
    }
}
fn nonempty(v: &str) -> Value {
    if v.trim().is_empty() {
        Value::Null
    } else {
        Value::String(v.trim().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn use_prompt_payload_uses_current_editor_content_and_hint() {
        let panel = PromptsPanel {
            selected: Some(0),
            name: "review".into(),
            content: "Review $ARGUMENTS".into(),
            argument_hint: "file or change".into(),
            ..PromptsPanel::default()
        };

        assert_eq!(
            panel.editor_payload(),
            json!({
                "name": "review",
                "content": "Review $ARGUMENTS",
                "argumentHint": "file or change"
            })
        );
    }
}
