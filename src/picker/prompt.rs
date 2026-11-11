//! A prompt: a page that asks for a line of text and runs a Lua action with
//! it. An `Argument` definition opens one.
use super::{Action, CustomAction, config, font};

pub(crate) struct Prompt {
    context: egui::Context,
    shared: nfm_egui::Prompt,
    position: config::Position,
    pub(crate) scope: config::Scope,
    anchor_bounds: Option<egui::Rect>,
    pub(crate) cursor_row: Option<u32>,
    pub(crate) from_copy: bool,
    pub(crate) typography: font::Typography,
    /// Runs with the text typed as its item.
    callback: std::rc::Rc<crate::lua_callbacks::Callback>,
    request_params: std::collections::BTreeMap<String, String>,
}

impl Prompt {
    /// The `query` parameter is the text the prompt opens with.
    pub(crate) fn start(
        ctx: egui::Context,
        from_copy: bool,
        options: config::PromptOptions,
    ) -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let defaults = nfm_egui::PromptConfig::default();
        Self {
            context: ctx,
            shared: nfm_egui::Prompt::new(
                ("nterm-prompt", id),
                nfm_egui::PromptConfig {
                    prompt: options.prompt.unwrap_or(defaults.prompt),
                    placeholder: options.placeholder.unwrap_or_default(),
                    initial_text: options
                        .params
                        .get("query")
                        .map(|query| query.chars().take(4096).collect())
                        .unwrap_or_default(),
                },
            ),
            position: options.position,
            scope: options.scope,
            anchor_bounds: None,
            cursor_row: None,
            from_copy,
            typography: font::Typography::default(),
            callback: options.callback,
            request_params: options.params,
        }
    }
    pub(crate) fn label(&self) -> &'static str {
        "ARGUMENT"
    }
    #[cfg(test)]
    pub(crate) fn text(&self) -> &str {
        self.shared.text()
    }
    pub(crate) fn surrender_focus(&self) {
        self.context.memory_mut(|memory| {
            if let Some(id) = memory.focused() {
                memory.surrender_focus(id);
            }
        });
        self.context.request_repaint();
    }
    /// Draw the prompt and report what the user did: Escape, or Enter, which
    /// runs the action with the text. The prompt stays as it is either way;
    /// the pane says what comes next.
    pub(crate) fn show_scoped(
        &mut self,
        ctx: &egui::Context,
        pane: egui::Rect,
        workspace: Option<egui::Rect>,
        interactive: bool,
    ) -> Option<Action> {
        self.anchor_bounds = Some(pane);
        let bounds = if self.scope == config::Scope::Workspace {
            workspace.unwrap_or(pane)
        } else {
            pane
        };
        let appearance = super::appearance(&self.typography, ctx);
        let placement = super::placement(
            self.position,
            self.cursor_row,
            self.anchor_bounds,
            self.typography.row_height,
            bounds,
        );
        match self
            .shared
            .show(ctx, placement, &appearance, !interactive)
            .event
        {
            Some(nfm_egui::PromptEvent::Cancelled) => Some(Action::Cancel),
            // The text typed is the item.
            Some(nfm_egui::PromptEvent::Submitted(text)) => Some(Action::Custom(CustomAction {
                callback: self.callback.clone(),
                text: text.clone(),
                filesystem: false,
                range: None,
                value: text,
                params: self.request_params.clone(),
                // Text is not a result: the prompt stays unless the action
                // closes it.
                accepted: false,
            })),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(ctx: &egui::Context, prompt: &mut Prompt, events: Vec<egui::Event>) -> Option<Action> {
        let mut action = None;
        let mut output = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 800.0),
                )),
                events,
                ..Default::default()
            },
            |ui| {
                // A discarded pass is run again; keep the action of either.
                let shown = prompt.show_scoped(ui.ctx(), ui.ctx().content_rect(), None, true);
                if shown.is_some() {
                    action = shown;
                }
            },
        );
        output.textures_delta.clear();
        action
    }
    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn callback() -> std::rc::Rc<crate::lua_callbacks::Callback> {
        let lua = std::rc::Rc::new(mlua::Lua::new());
        let function = lua
            .load("return function() end")
            .eval::<mlua::Function>()
            .unwrap();
        std::rc::Rc::new(crate::lua_callbacks::Callback {
            key: lua.create_registry_value(function).unwrap(),
            lua,
            name: "fixture".into(),
        })
    }
    fn options(
        callback: std::rc::Rc<crate::lua_callbacks::Callback>,
        params: std::collections::BTreeMap<String, String>,
    ) -> config::PromptOptions {
        config::PromptOptions {
            scope: Default::default(),
            position: Default::default(),
            callback,
            params,
            prompt: Some("Grep:".into()),
            placeholder: None,
        }
    }

    #[test]
    fn a_prompt_runs_its_action_with_the_text_typed_and_stays_as_it_was() {
        let ctx = egui::Context::default();
        let callback = callback();
        let mut prompt = Prompt::start(
            ctx.clone(),
            false,
            options(callback.clone(), [("cwd".into(), "C:\\src".into())].into()),
        );
        assert_eq!(prompt.label(), "ARGUMENT");
        frame(&ctx, &mut prompt, vec![]);
        frame(&ctx, &mut prompt, vec![]);
        let action = frame(
            &ctx,
            &mut prompt,
            vec![
                egui::Event::Text("fn \"main\" ".into()),
                key(egui::Key::Enter),
            ],
        );
        let Some(Action::Custom(action)) = action else {
            panic!("the action did not run");
        };
        assert!(std::rc::Rc::ptr_eq(&action.callback, &callback));
        assert_eq!(
            (action.text.as_str(), action.value.as_str()),
            ("fn \"main\" ", "fn \"main\" ")
        );
        assert!(!action.filesystem && action.range.is_none() && !action.accepted);
        assert_eq!(action.params["cwd"], "C:\\src");
        // It stays on screen while its action runs, as it was, for a page
        // on top to come back to.
        assert_eq!(prompt.text(), "fn \"main\" ");

        let action = frame(&ctx, &mut prompt, vec![key(egui::Key::Escape)]);
        assert!(matches!(action, Some(Action::Cancel)));
        assert_eq!(prompt.text(), "fn \"main\" ");
    }

    #[test]
    fn a_prompt_opens_with_the_query_parameter_as_its_text() {
        let ctx = egui::Context::default();
        let prompt = Prompt::start(
            ctx,
            true,
            options(callback(), [("query".into(), "café".into())].into()),
        );
        assert_eq!(prompt.text(), "café");
        assert!(prompt.from_copy);
    }
}
