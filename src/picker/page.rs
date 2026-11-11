//! What a pane shows over its terminal: a picker, a prompt asking for text,
//! or what a program prints. The pane keeps them in a stack and treats them alike for where they
//! were opened and what they report; what only a picker does is asked of the
//! picker.
use super::{Action, Output, Picker, Prompt, config, font};

#[allow(clippy::large_enum_variant)]
pub(crate) enum Page {
    Picker(Picker),
    Prompt(Prompt),
    Output(Output),
}
impl From<Picker> for Page {
    fn from(picker: Picker) -> Self {
        Self::Picker(picker)
    }
}
impl From<Prompt> for Page {
    fn from(prompt: Prompt) -> Self {
        Self::Prompt(prompt)
    }
}
impl From<Output> for Page {
    fn from(output: Output) -> Self {
        Self::Output(output)
    }
}

impl Page {
    pub(crate) fn picker(&self) -> Option<&Picker> {
        match self {
            Self::Picker(picker) => Some(picker),
            Self::Prompt(_) | Self::Output(_) => None,
        }
    }
    pub(crate) fn picker_mut(&mut self) -> Option<&mut Picker> {
        match self {
            Self::Picker(picker) => Some(picker),
            Self::Prompt(_) | Self::Output(_) => None,
        }
    }
    pub(crate) fn into_picker(self) -> Option<Picker> {
        match self {
            Self::Picker(picker) => Some(picker),
            Self::Prompt(_) | Self::Output(_) => None,
        }
    }
    /// The terminal snapshot the page reads, if it reads one.
    pub(crate) fn generation(&self) -> Option<u64> {
        self.picker().and_then(|picker| picker.generation)
    }
    /// Whether it was opened from copy mode, which closing it returns to.
    pub(crate) fn from_copy(&self) -> bool {
        match self {
            Self::Picker(picker) => picker.from_copy,
            Self::Prompt(prompt) => prompt.from_copy,
            Self::Output(output) => output.from_copy,
        }
    }
    pub(crate) fn scope(&self) -> config::Scope {
        match self {
            Self::Picker(picker) => picker.scope,
            Self::Prompt(prompt) => prompt.scope,
            Self::Output(output) => output.scope,
        }
    }
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Picker(picker) => picker.label(),
            Self::Prompt(prompt) => prompt.label(),
            Self::Output(output) => output.label(),
        }
    }
    pub(crate) fn set_typography(&mut self, typography: font::Typography) {
        match self {
            Self::Picker(picker) => picker.typography = typography,
            Self::Prompt(prompt) => prompt.typography = typography,
            Self::Output(output) => output.typography = typography,
        }
    }
    /// The terminal row a `Cursor` position opens beneath.
    pub(crate) fn set_cursor_row(&mut self, row: Option<u32>) {
        match self {
            Self::Picker(picker) => picker.cursor_row = row,
            Self::Prompt(prompt) => prompt.cursor_row = row,
            Self::Output(output) => output.cursor_row = row,
        }
    }
    pub(crate) fn surrender_focus(&self) {
        match self {
            Self::Picker(picker) => picker.surrender_focus(),
            Self::Prompt(prompt) => prompt.surrender_focus(),
            Self::Output(output) => output.surrender_focus(),
        }
    }
    pub(crate) fn show_scoped(
        &mut self,
        ctx: &egui::Context,
        pane: egui::Rect,
        workspace: Option<egui::Rect>,
        interactive: bool,
    ) -> Option<Action> {
        match self {
            Self::Picker(picker) => picker.show_scoped(ctx, pane, workspace, interactive),
            Self::Prompt(prompt) => prompt.show_scoped(ctx, pane, workspace, interactive),
            Self::Output(output) => output.show_scoped(ctx, pane, workspace, interactive),
        }
    }
}
