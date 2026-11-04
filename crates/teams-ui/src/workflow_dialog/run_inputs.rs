//! Payload-bearing manual triggers use the definition’s actual fields. Inputs remain native; no credential or
//! request body goes to a service-returned callback. The engine validates the freshly read schema again before run.
use super::*;

pub(super) struct RunField {
    key: String,
    label: String,
    description: String,
    schema: Value,
    required: bool,
    input: Entity<InputState>,
}
impl WorkflowDialog {
    pub(super) fn prepare_run(&mut self, card: &Card, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.core.read_only() || self.uncertain.is_some() {
            return;
        }
        let triggers = card
            .properties
            .pointer("/definition/triggers")
            .and_then(Value::as_object);
        let Some((trigger, definition)) = triggers
            .filter(|triggers| triggers.len() == 1)
            .and_then(|triggers| triggers.iter().next())
        else {
            self.error = Some("This workflow does not have one runnable trigger. Refresh its definition.".to_owned());
            cx.notify();
            return;
        };
        let schema = definition.pointer("/inputs/schema");
        let properties = schema
            .and_then(|schema| schema.get("properties"))
            .and_then(Value::as_object);
        if properties.is_some_and(|properties| !properties.is_empty())
            || schema
                .and_then(|schema| schema.get("required"))
                .and_then(Value::as_array)
                .is_some_and(|required| !required.is_empty())
        {
            self.prepare_run_fields(schema.unwrap_or(&Value::Null), window, cx);
            self.screen = Screen::RunInputs {
                card: card.clone(),
                trigger: trigger.clone(),
                schema: schema.cloned().unwrap_or(Value::Null),
            };
            self.error = None;
            cx.notify();
            return;
        }
        let Some(environment) = self.environment.clone() else {
            return;
        };
        self.mutate(
            WorkflowRequest::Run {
                environment,
                flow: card.id.clone(),
                trigger: trigger.clone(),
            },
            "Workflow run requested",
            cx,
        );
    }
    fn prepare_run_fields(&mut self, schema: &Value, window: &mut Window, cx: &mut Context<Self>) {
        let required = schema.get("required").and_then(Value::as_array);
        self.run_fields = schema
            .get("properties")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(key, field)| {
                let default = field.get("default").map_or_else(String::new, |value| {
                    value.as_str().map_or_else(|| value.to_string(), str::to_owned)
                });
                RunField {
                    key: key.clone(),
                    label: field.get("title").and_then(Value::as_str).unwrap_or(key).to_owned(),
                    description: field
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    schema: field.clone(),
                    required: required.is_some_and(|keys| keys.iter().any(|value| value.as_str() == Some(key))),
                    input: cx.new(|cx| InputState::new(window, cx).default_value(default)),
                }
            })
            .collect();
    }
    pub(super) fn run_inputs_view(&self, card: &Card, palette: Palette, cx: &Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(format!("Run {}", card.name)),
            )
            .child("Enter this workflow’s trigger inputs. Fields marked * are required.")
            .children(self.run_fields.iter().enumerate().map(|(index, field)| {
                let kind = field.schema.get("type").and_then(Value::as_str).unwrap_or("value");
                let kind_help = match kind {
                    "string" => "",
                    "integer" => "Enter a whole number.",
                    "number" => "Enter a number.",
                    "boolean" => "Enter true or false.",
                    "object" => "Enter a JSON object.",
                    "array" => "Enter a JSON list.",
                    _ => "Enter a JSON value.",
                };
                let help = format!("{} {}", field.description, kind_help).trim().to_owned();
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(format!("{}{}", field.label, if field.required { " *" } else { "" }))
                    .child(selected_by(
                        format!("workflow-run-input-{index}"),
                        selected_by(format!("workflow-run-input-{}", field.key), Input::new(&field.input)),
                    ))
                    .child(div().text_sm().text_color(rgb(palette.muted_foreground)).child(help))
            }))
            .child(
                div().flex().justify_end().child(selected_by(
                    "workflow-run-submit",
                    Button::new("workflow-run-submit")
                        .primary()
                        .label("Run workflow")
                        .icon(Glyph::PLAY.icon())
                        .disabled(self.core.read_only() || self.busy || self.uncertain.is_some())
                        .on_click(cx.listener(|this, _, _, cx| this.submit_run_inputs(cx))),
                )),
            )
            .into_any_element()
    }
    fn submit_run_inputs(&mut self, cx: &mut Context<Self>) {
        let Screen::RunInputs { card, trigger, schema } = &self.screen else {
            return;
        };
        let Some(environment) = self.environment.clone() else {
            return;
        };
        let inputs = match self.run_input_values(schema, cx) {
            Ok(inputs) => inputs,
            Err(message) => {
                self.error = Some(message.to_owned());
                cx.notify();
                return;
            }
        };
        self.mutate(
            WorkflowRequest::RunWithInputs {
                environment,
                flow: card.id.clone(),
                trigger: trigger.clone(),
                inputs,
            },
            "Workflow run requested",
            cx,
        );
    }
    fn run_input_values(&self, schema: &Value, cx: &Context<Self>) -> Result<Value, &'static str> {
        let mut values = serde_json::Map::new();
        for field in &self.run_fields {
            let value = field.input.read(cx).value();
            if value.is_empty() {
                if field.required {
                    return Err("Enter a value for every required trigger input.");
                }
                continue;
            }
            let value = if field.schema.get("type").and_then(Value::as_str) == Some("string") {
                Value::String(value.to_string())
            } else {
                serde_json::from_str(value.as_ref()).map_err(
                    |_| "A trigger input has the wrong type. Use a number, true/false, object or list as shown.",
                )?
            };
            values.insert(field.key.clone(), value);
        }
        let value = Value::Object(values);
        if !teams_proto::workflow::valid_trigger_inputs(schema, &value) {
            return Err(
                "The inputs do not match this workflow’s required fields, types or allowed values. Check its configuration.",
            );
        }
        Ok(value)
    }
}
