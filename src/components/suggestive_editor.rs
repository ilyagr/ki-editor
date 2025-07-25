use crate::app::{Dispatch, Dispatches};
use crate::context::{Context, GlobalMode};
use crate::grid::StyleKey;
use crate::selection::SelectionMode;
use DispatchEditor::*;

use crate::selection_range::SelectionRange;
use crate::{
    buffer::Buffer,
    lsp::completion::{Completion, CompletionItem},
};

use itertools::Itertools;
use my_proc_macros::key;
use std::{cell::RefCell, rc::Rc};

use super::dropdown::{Dropdown, DropdownConfig};
use super::editor::{Direction, DispatchEditor, IfCurrentNotFound};
use super::editor_keymap::Meaning;
use super::keymap_legend::{Keymap, Keymaps};
use super::{
    component::Component,
    dropdown::DropdownItem,
    editor::{Editor, Mode},
};

/// Editor with auto-complete
pub(crate) struct SuggestiveEditor {
    editor: Editor,
    completion_dropdown: Dropdown,
    trigger_characters: Vec<String>,
    filter: SuggestiveEditorFilter,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SuggestiveEditorFilter {
    CurrentWord,
    CurrentLine,
}

impl From<CompletionItem> for DropdownItem {
    fn from(item: CompletionItem) -> Self {
        DropdownItem::new(format!("{} {}", item.emoji(), item.label()))
            .set_info(item.info())
            .set_dispatches(item.dispatches())
            .set_on_focused(Dispatches::one(Dispatch::ResolveCompletionItem(
                item.completion_item(),
            )))
    }
}

impl Component for SuggestiveEditor {
    fn editor(&self) -> &Editor {
        &self.editor
    }

    fn editor_mut(&mut self) -> &mut Editor {
        &mut self.editor
    }

    fn handle_dispatch_editor(
        &mut self,
        context: &mut Context,
        dispatch: DispatchEditor,
    ) -> anyhow::Result<Dispatches> {
        let dispatches = self
            .editor_mut()
            .handle_dispatch_editor(context, dispatch)?;
        let update_filter_result = self.update_filter();
        Ok(dispatches.chain(update_filter_result?))
    }

    fn handle_key_event(
        &mut self,
        context: &Context,
        event: event::KeyEvent,
    ) -> anyhow::Result<Dispatches> {
        if self.editor.mode == Mode::Insert && self.completion_dropdown_opened() {
            if let Some(keymap) = completion_item_keymaps(context).get(&event) {
                log::info!("dispatches = {:?}", keymap.get_dispatches());
                return Ok(keymap.get_dispatches());
            };
            match event {
                key!("down") => return self.next_completion_item(),
                key!("up") => return self.previous_completion_item(),
                key!("tab") => return self.select_completion_item(),

                _ => {}
            }
        }

        // Every other character typed in Insert mode should update the dropdown to show
        // relevant completions.
        let dispatches = self.editor.handle_key_event(context, event.clone())?;

        let render_dropdown_dispatch = self.update_filter()?;
        Ok(render_dropdown_dispatch
            .chain(dispatches)
            .chain(match event {
                key!("esc") => [
                    Dispatch::CloseDropdown,
                    Dispatch::CloseEditorInfo,
                    Dispatch::ToEditor(EnterNormalMode),
                ]
                .into_iter()
                .chain(match (context.mode(), context.quickfix_list_state()) {
                    (Some(GlobalMode::QuickfixListItem), Some(state)) => {
                        Some(Dispatch::ToEditor(SetSelectionMode(
                            IfCurrentNotFound::LookForward,
                            SelectionMode::LocalQuickfix {
                                title: state.title.clone(),
                            },
                        )))
                    }
                    _ => None,
                })
                .collect_vec()
                .into(),
                _ if self.editor.mode == Mode::Insert => {
                    vec![Dispatch::RequestCompletion, Dispatch::RequestSignatureHelp].into()
                }
                _ => Default::default(),
            }))
    }
}

impl SuggestiveEditor {
    pub(crate) fn from_buffer(buffer: Rc<RefCell<Buffer>>, filter: SuggestiveEditorFilter) -> Self {
        Self {
            editor: Editor::from_buffer(buffer),
            completion_dropdown: Dropdown::new(DropdownConfig {
                title: "Completion".to_string(),
            }),
            trigger_characters: vec![],
            filter,
        }
    }

    pub(crate) fn handle_dispatch(
        &mut self,
        dispatch: DispatchSuggestiveEditor,
    ) -> anyhow::Result<Dispatches> {
        match dispatch {
            #[cfg(test)]
            DispatchSuggestiveEditor::CompletionFilter(filter) => {
                self.filter = filter;
                Ok(Default::default())
            }
            DispatchSuggestiveEditor::Completion(completion) => {
                if self.editor.mode == Mode::Insert {
                    self.set_completion(completion);
                    Ok(self.render_completion_dropdown(false))
                } else {
                    Ok(Vec::new().into())
                }
            }
            DispatchSuggestiveEditor::UpdateCurrentCompletionItem(completion_item) => {
                Ok(self.update_current_completion_item(completion_item))
            }
            DispatchSuggestiveEditor::MoveToCompletionItem(Direction::End) => {
                self.next_completion_item()
            }
            DispatchSuggestiveEditor::MoveToCompletionItem(Direction::Start) => {
                self.previous_completion_item()
            }
            DispatchSuggestiveEditor::SelectCompletionItem => self.select_completion_item(),
        }
    }

    pub(crate) fn completion_dropdown_current_item(&mut self) -> Option<DropdownItem> {
        self.completion_dropdown.current_item()
    }

    pub(crate) fn all_filtered_items(&mut self) -> Vec<DropdownItem> {
        self.completion_dropdown.all_filtered_items()
    }

    pub(crate) fn completion_dropdown_opened(&self) -> bool {
        !self.completion_dropdown.items().is_empty()
    }

    pub(crate) fn set_completion(&mut self, completion: Completion) {
        self.completion_dropdown.set_items(completion.items);
        self.trigger_characters = completion.trigger_characters;
    }

    pub(crate) fn render_completion_dropdown(&self, ignore_insert_mode: bool) -> Dispatches {
        log::info!(
            "ignore_insert_mode = {ignore_insert_mode} mode = {:?}",
            self.editor.mode
        );
        if (!ignore_insert_mode && self.editor.mode != Mode::Insert)
            || self.completion_dropdown.no_matching_candidates()
        {
            Dispatches::one(Dispatch::CloseDropdown)
        } else {
            let on_focused = self
                .completion_dropdown
                .current_item()
                .map(|item| {
                    if item.resolved() {
                        Default::default()
                    } else {
                        item.on_focused()
                    }
                })
                .unwrap_or_default();
            Dispatches::one(Dispatch::RenderDropdown {
                render: self.completion_dropdown.render(),
            })
            .chain(on_focused)
        }
    }

    fn update_filter(&mut self) -> anyhow::Result<Dispatches> {
        if self.editor.mode != Mode::Insert {
            return Ok(Dispatches::empty());
        }
        let filter = match self.filter {
            SuggestiveEditorFilter::CurrentWord => {
                // We need to subtract 1 because we need to get the character
                // before the cursor, not the character at the cursor
                let cursor_position = self.editor().get_cursor_position()?.sub_column(1);
                match self.editor().buffer().get_char_at_position(cursor_position) {
                    // The filter should be empty if the current character is a trigger
                    // character, so that we can show all the completion items.
                    Some(current_char)
                        if self.trigger_characters.contains(&current_char.to_string()) =>
                    {
                        "".to_string()
                    }

                    // If the current character is not a trigger character, we should
                    // filter based on the current word under the cursor.
                    _ => self.editor.get_current_word()?,
                }
            }
            SuggestiveEditorFilter::CurrentLine => self.editor().current_line()?,
        };

        self.completion_dropdown.set_filter(&filter);

        let render_completion_dropdown = self.render_completion_dropdown(false);
        Ok(render_completion_dropdown)
    }

    fn update_current_completion_item(&mut self, completion_item: CompletionItem) -> Dispatches {
        self.completion_dropdown
            .update_current_item(completion_item.into());
        self.render_completion_dropdown(false)
    }

    fn previous_completion_item(&mut self) -> Result<Dispatches, anyhow::Error> {
        self.completion_dropdown.previous_item();
        Ok(self.render_completion_dropdown(false))
    }

    fn next_completion_item(&mut self) -> Result<Dispatches, anyhow::Error> {
        self.completion_dropdown.next_item();
        let dispatches = self.render_completion_dropdown(false);
        log::info!("next_compl = {:?}", dispatches);
        Ok(self.render_completion_dropdown(false))
    }

    fn select_completion_item(&mut self) -> Result<Dispatches, anyhow::Error> {
        let current_item = self.completion_dropdown.current_item();
        if let Some(completion) = current_item {
            self.completion_dropdown.set_items(Vec::new());
            Ok(Dispatches::one(Dispatch::CloseDropdown).chain(completion.dispatches))
        } else {
            Ok(Default::default())
        }
    }

    pub(crate) fn update_current_line(
        &mut self,
        context: &Context,
        display: &str,
    ) -> anyhow::Result<Dispatches> {
        self.editor_mut().update_current_line(context, display)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DispatchSuggestiveEditor {
    #[cfg(test)]
    CompletionFilter(SuggestiveEditorFilter),
    Completion(Completion),
    UpdateCurrentCompletionItem(CompletionItem),
    MoveToCompletionItem(Direction),
    SelectCompletionItem,
}

#[cfg(test)]
mod test_suggestive_editor {
    use crate::components::editor::{DispatchEditor::*, IfCurrentNotFound};
    use crate::components::suggestive_editor::DispatchSuggestiveEditor::*;
    use crate::context::Context;
    use crate::lsp::completion::{CompletionItemEdit, PositionalEdit};
    use crate::lsp::documentation::Documentation;
    use crate::position::Position;
    use crate::selection::SelectionMode;
    use crate::{
        app::Dispatch,
        buffer::{Buffer, BufferOwner},
        components::{component::Component, editor::Direction},
        lsp::completion::{Completion, CompletionItem},
        test_app::execute_test,
        test_app::ExpectKind::*,
        test_app::Step::*,
    };
    use lsp_types::{CompletionItemKind, CompletionTextEdit, TextEdit};
    use my_proc_macros::{key, keys};
    use shared::canonicalized_path::CanonicalizedPath;
    use std::{cell::RefCell, rc::Rc};
    use Dispatch::*;

    use super::{Info, SuggestiveEditor, SuggestiveEditorFilter};

    fn dummy_completion() -> Completion {
        Completion {
            trigger_characters: vec![".".to_string()],
            items: vec![
                CompletionItem::from_label("Spongebob".to_string()),
                CompletionItem::from_label("Patrick".to_string()),
                CompletionItem::from_label("Squidward".to_string()),
            ]
            .into_iter()
            .map(|item| item.into())
            .collect(),
        }
    }

    fn editor(filter: SuggestiveEditorFilter) -> SuggestiveEditor {
        SuggestiveEditor::from_buffer(Rc::new(RefCell::new(Buffer::new(None, ""))), filter)
    }

    #[test]
    fn typing_in_insert_mode_should_request_completion() {
        let mut editor = editor(SuggestiveEditorFilter::CurrentWord);

        let file = tempfile::NamedTempFile::new().unwrap();

        let path: CanonicalizedPath = file.path().to_path_buf().try_into().unwrap();

        editor.editor_mut().buffer_mut().set_path(path);
        // Enter insert mode
        let _ = editor
            .editor_mut()
            .enter_insert_mode(Direction::Start, &Context::default())
            .unwrap();

        // Type something
        let dispatches = editor.handle_events(keys!("p")).unwrap();

        // Expect the completion request to be sent
        assert!(dispatches
            .into_vec()
            .into_iter()
            .any(|dispatch| matches!(&dispatch, Dispatch::RequestCompletion)));
    }

    #[test]
    fn entering_insert_mode_should_request_signature_help() {
        let mut editor = editor(SuggestiveEditorFilter::CurrentWord);

        let file = tempfile::NamedTempFile::new().unwrap();

        let path: CanonicalizedPath = file.path().to_path_buf().try_into().unwrap();

        editor.editor_mut().buffer_mut().set_path(path);

        // Enter insert mode
        let dispatches = editor
            .editor_mut()
            .enter_insert_mode(Direction::Start, &Context::default())
            .unwrap();

        // Expect the signature help request to be sent
        assert!(dispatches
            .into_vec()
            .into_iter()
            .any(|dispatch| matches!(&dispatch, Dispatch::RequestSignatureHelp)));
    }

    #[test]
    fn completion_without_edit_1() -> Result<(), anyhow::Error> {
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent("".to_string())),
                Editor(EnterInsertMode(Direction::Start)),
                SuggestiveEditor(CompletionFilter(SuggestiveEditorFilter::CurrentWord)),
                // Pretend that the LSP server returned a completion
                SuggestiveEditor(Completion(dummy_completion())),
                // Expect the completion dropdown to be open,
                Expect(CompletionDropdownContent(
                    " Patrick\n Spongebob\n Squidward",
                )),
                // Type in 'pa'
                App(HandleKeyEvents(keys!("p a").to_vec())),
                // Expect the dropdown items to be filtered
                Expect(CompletionDropdownIsOpen(true)),
                Expect(CompletionDropdownContent(" Patrick")),
                App(HandleKeyEvent(key!("tab"))),
                // Expect the buffer to contain the selected item
                Expect(CurrentComponentContent("Patrick")),
                Expect(CompletionDropdownIsOpen(false)),
            ])
        })
    }

    #[test]
    /// Should not replace non-alphanumeric word
    fn completion_without_edit_2() -> Result<(), anyhow::Error> {
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent("".to_string())),
                Editor(EnterInsertMode(Direction::Start)),
                SuggestiveEditor(CompletionFilter(SuggestiveEditorFilter::CurrentWord)),
                // Pretend that the LSP server returned a completion
                SuggestiveEditor(Completion(dummy_completion())),
                // Type in '.',,
                App(HandleKeyEvents(keys!("a .").to_vec())),
                Expect(CurrentComponentContent("a.")),
                App(HandleKeyEvent(key!("tab"))),
                Expect(CurrentComponentContent("a.Patrick")),
            ])
        })
    }

    #[test]
    /// Should replace token, not word
    fn completion_without_edit_3() -> Result<(), anyhow::Error> {
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent("".to_string())),
                Editor(EnterInsertMode(Direction::Start)),
                SuggestiveEditor(CompletionFilter(SuggestiveEditorFilter::CurrentWord)),
                // Pretend that the LSP server returned a completion
                SuggestiveEditor(Completion(Completion {
                    trigger_characters: vec![".".to_string()],
                    items: vec![CompletionItem::from_label("aBigCatDog".to_string())]
                        .into_iter()
                        .map(|item| item.into())
                        .collect(),
                })),
                // Type in 'aBigCat'
                Editor(Insert("aBigCat".to_string())),
                Expect(EditorCursorPosition(Position::new(0, 7))),
                App(HandleKeyEvent(key!("tab"))),
                Expect(CurrentComponentContent("aBigCatDog")),
            ])
        })
    }

    #[test]
    /// Should work when surrounded by parenthesis
    fn completion_without_edit_4() -> Result<(), anyhow::Error> {
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent("()".to_string())),
                Editor(EnterInsertMode(Direction::Start)),
                Editor(MatchLiteral("(".to_string())),
                SuggestiveEditor(CompletionFilter(SuggestiveEditorFilter::CurrentWord)),
                // Pretend that the LSP server returned a completion
                SuggestiveEditor(Completion(Completion {
                    trigger_characters: vec![".".to_string()],
                    items: vec![CompletionItem::from_label("aBigCatDog".to_string())]
                        .into_iter()
                        .map(|item| item.into())
                        .collect(),
                })),
                Editor(EnterInsertMode(Direction::End)),
                // Type in 'aBigCat',,
                Editor(Insert("aBigCat".to_string())),
                App(HandleKeyEvent(key!("tab"))),
                Expect(CurrentComponentContent("(aBigCatDog)")),
            ])
        })
    }

    #[test]
    /// Should use `insert_text` if it is defined
    fn completion_without_edit_5() -> Result<(), anyhow::Error> {
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent("hello".to_string())),
                Editor(EnterInsertMode(Direction::Start)),
                Editor(MatchLiteral("hello".to_string())),
                SuggestiveEditor(CompletionFilter(SuggestiveEditorFilter::CurrentWord)),
                // Pretend that the LSP server returned a completion
                SuggestiveEditor(Completion(Completion {
                    trigger_characters: vec![".".to_string()],
                    items: [CompletionItem::from_label("aBigCatDog".to_string())
                        .set_insert_text(Some("harimau".to_string()))]
                    .into_iter()
                    .map(|item| item.into())
                    .collect(),
                })),
                Editor(EnterInsertMode(Direction::End)),
                Editor(Insert(" aBig".to_string())),
                App(HandleKeyEvent(key!("tab"))),
                Expect(CurrentComponentContent("hello harimau")),
            ])
        })
    }

    #[test]
    fn should_utilize_additional_edits() -> Result<(), anyhow::Error> {
        let test = |text_edit: Option<CompletionTextEdit>| {
            execute_test(move |s| {
                Box::new([
                    App(OpenFile {
                        path: s.main_rs(),
                        owner: BufferOwner::User,
                        focus: true,
                    }),
                    Editor(SetContent("hello".to_string())),
                    Editor(EnterInsertMode(Direction::Start)),
                    Editor(MatchLiteral("hello".to_string())),
                    SuggestiveEditor(CompletionFilter(SuggestiveEditorFilter::CurrentWord)),
                    // Pretend that the LSP server returned a completion
                    SuggestiveEditor(Completion(Completion {
                        trigger_characters: vec![".".to_string()],
                        items: [lsp_types::CompletionItem {
                            label: "aBigCat".to_string(),
                            text_edit: text_edit.clone(),
                            additional_text_edits: Some(
                                [
                                    TextEdit {
                                        range: lsp_types::Range::new(
                                            lsp_types::Position::new(0, 0),
                                            lsp_types::Position::new(0, 0),
                                        ),
                                        new_text: "import 'cats';".to_string(),
                                    },
                                    TextEdit {
                                        range: lsp_types::Range::new(
                                            lsp_types::Position::new(0, 5),
                                            lsp_types::Position::new(0, 5),
                                        ),
                                        new_text: "!".to_string(),
                                    },
                                ]
                                .to_vec(),
                            ),
                            ..Default::default()
                        }
                        .into()]
                        .into_iter()
                        .map(|item: CompletionItem| item.into())
                        .collect(),
                    })),
                    Editor(EnterInsertMode(Direction::End)),
                    Editor(Insert(" aBig".to_string())),
                    App(HandleKeyEvent(key!("tab"))),
                    Expect(CurrentComponentContent("import 'cats';hello! aBigCat")),
                ])
            })
        };
        // Case 1: without `text_edit`
        test(None)?;

        // Case 2: with `text_edit`
        test(Some(CompletionTextEdit::Edit(TextEdit {
            range: lsp_types::Range::new(
                lsp_types::Position::new(0, 6),
                lsp_types::Position::new(0, 10),
            ),
            new_text: "aBigCat".to_string(),
        })))?;
        Ok(())
    }

    #[test]
    fn update_current_completion_item() -> Result<(), anyhow::Error> {
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent("hello".to_string())),
                Editor(EnterInsertMode(Direction::Start)),
                Editor(MatchLiteral("hello".to_string())),
                SuggestiveEditor(CompletionFilter(SuggestiveEditorFilter::CurrentWord)),
                Editor(EnterInsertMode(Direction::End)),
                SuggestiveEditor(Completion(Completion {
                    trigger_characters: vec![".".to_string()],
                    items: [
                        CompletionItem::from_label("apple".to_string()),
                        CompletionItem::from_label("abanana".to_string()),
                    ]
                    .into_iter()
                    .map(|item: CompletionItem| item.into())
                    .collect(),
                })),
                Editor(Insert(" a".to_string())),
                App(HandleKeyEvent(key!("ctrl+n"))),
                // Update the current completion
                SuggestiveEditor(UpdateCurrentCompletionItem(
                    CompletionItem::from_label("abanana".to_string())
                        .set_insert_text(Some("apisang".to_string())),
                )),
                App(HandleKeyEvent(key!("tab"))),
                Expect(CurrentComponentContent("hello apisang")),
            ])
        })
    }

    #[test]
    fn completion_info_documentation() -> anyhow::Result<()> {
        let completion_item = |label: &str, documentation: Option<&str>| CompletionItem {
            label: label.to_string(),
            edit: Some(CompletionItemEdit::PositionalEdit(PositionalEdit {
                range: Position::new(0, 0)..Position::new(0, 6),
                new_text: label.to_string(),
            })),
            documentation: documentation.map(Documentation::new),
            sort_text: None,
            insert_text: None,
            kind: None,
            detail: None,
            completion_item: Default::default(),
        };
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent("".to_string())),
                Editor(EnterInsertMode(Direction::Start)),
                SuggestiveEditor(CompletionFilter(SuggestiveEditorFilter::CurrentWord)),
                // Pretend that the LSP server returned a completion
                SuggestiveEditor(super::DispatchSuggestiveEditor::Completion(Completion {
                    trigger_characters: vec![".".to_string()],
                    items: vec![
                        completion_item("Spongebob", Some("krabby patty maker")),
                        completion_item("Zatrick Mazerick", None),
                    ]
                    .into_iter()
                    .map(|item| item.into())
                    .collect(),
                })),
                // Expect the "Completion Info" panel is shown, because "Spongebob" has doc
                Expect(AppGridContains("Completion Info")),
                Expect(AppGridContains("patty maker")),
                App(HandleKeyEvents(keys!("Z a t r i c k").to_vec())),
                Expect(AppGridContains("atrick")),
                // Expect the "Completion Info" panel is hidden, because "patrick" has no doc
                Expect(Not(Box::new(AppGridContains("Completion Info")))),
            ])
        })
    }

    #[test]
    /// If there are multiple cursors, then Edit should be ignored
    /// and replace the words under cursors with the completion label instead.
    /// This is because the Edit range is only applicable to the primary cursor.
    fn completion_with_edit_multicursor() -> anyhow::Result<()> {
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent("foo bar spam".to_string())),
                Editor(SetSelectionMode(
                    IfCurrentNotFound::LookForward,
                    crate::selection::SelectionMode::Word,
                )),
                Editor(CursorAddToAllSelections),
                Editor(EnterInsertMode(Direction::End)),
                SuggestiveEditor(CompletionFilter(SuggestiveEditorFilter::CurrentWord)),
                // Pretend that the LSP server returned a completion
                SuggestiveEditor(Completion(Completion {
                    trigger_characters: vec![".".to_string()],
                    items: vec![CompletionItem {
                        label: "foochuk".to_string(),
                        edit: Some(CompletionItemEdit::PositionalEdit(PositionalEdit {
                            range: Position::new(0, 0)..Position::new(0, 3),
                            new_text: "foochuk".to_string(),
                        })),
                        documentation: None,
                        sort_text: None,
                        kind: None,
                        detail: None,
                        insert_text: None,
                        completion_item: Default::default(),
                    }]
                    .into_iter()
                    .map(|item| item.into())
                    .collect(),
                })),
                App(HandleKeyEvent(key!("tab"))),
                Expect(CurrentComponentContent("foochuk foochuk foochuk")),
            ])
        })
    }

    #[test]
    fn completion_with_edit() -> anyhow::Result<()> {
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent("".to_string())),
                Editor(EnterInsertMode(Direction::Start)),
                SuggestiveEditor(CompletionFilter(SuggestiveEditorFilter::CurrentWord)),
                App(HandleKeyEvents(keys!("s p o n g e").to_vec())),
                // Pretend that the LSP server returned a completion
                SuggestiveEditor(Completion(Completion {
                    trigger_characters: vec![".".to_string()],
                    items: vec![CompletionItem {
                        label: "Spongebob".to_string(),
                        edit: Some(CompletionItemEdit::PositionalEdit(PositionalEdit {
                            range: Position::new(0, 0)..Position::new(0, 6),
                            new_text: "Spongebob".to_string(),
                        })),
                        documentation: None,
                        sort_text: None,
                        kind: None,
                        detail: None,
                        insert_text: None,
                        completion_item: Default::default(),
                    }]
                    .into_iter()
                    .map(|item| item.into())
                    .collect(),
                })),
                App(HandleKeyEvent(key!("tab"))),
                Expect(CurrentComponentContent("Spongebob")),
                App(HandleKeyEvents(keys!("e n d").to_vec())),
                Expect(CurrentComponentContent("Spongebobend")),
            ])
        })
    }

    #[test]
    fn navigate_dropdown() -> anyhow::Result<()> {
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent("".to_string())),
                SuggestiveEditor(CompletionFilter(SuggestiveEditorFilter::CurrentWord)),
                Editor(EnterInsertMode(Direction::Start)),
                SuggestiveEditor(Completion(dummy_completion())),
                Expect(CompletionDropdownIsOpen(true)),
                Expect(CompletionDropdownSelectedItem("Patrick")),
                App(HandleKeyEvent(key!("down"))),
                Expect(CompletionDropdownSelectedItem("Spongebob")),
                App(HandleKeyEvent(key!("up"))),
                Expect(CompletionDropdownSelectedItem("Patrick")),
                App(HandleKeyEvent(key!("alt+k"))),
                Expect(CompletionDropdownSelectedItem("Spongebob")),
                App(HandleKeyEvent(key!("alt+i"))),
                Expect(CompletionDropdownSelectedItem("Patrick")),
            ])
        })
    }

    #[test]
    fn trigger_characters() -> Result<(), anyhow::Error> {
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent("".to_string())),
                SuggestiveEditor(CompletionFilter(SuggestiveEditorFilter::CurrentWord)),
                Editor(EnterInsertMode(Direction::Start)),
                SuggestiveEditor(Completion(dummy_completion())),
                App(HandleKeyEvents(keys!("p a").to_vec())),
                Expect(CompletionDropdownIsOpen(true)),
                Expect(CompletionDropdownContent(" Patrick")),
                // Type in one of the trigger characters, '.'
                App(HandleKeyEvent(key!("."))),
                Expect(CompletionDropdownIsOpen(true)),
                // Expect dropdown items to be unfiltered (showing all items)
                Expect(CompletionDropdownContent(
                    " Patrick\n Spongebob\n Squidward",
                )),
            ])
        })
    }

    #[test]
    fn enter_normal_mode_should_close_completion_dropdown() -> anyhow::Result<()> {
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                SuggestiveEditor(CompletionFilter(SuggestiveEditorFilter::CurrentWord)),
                Editor(EnterInsertMode(Direction::Start)),
                SuggestiveEditor(Completion(dummy_completion())),
                Expect(CompletionDropdownIsOpen(true)),
                Expect(CurrentPath(s.main_rs())),
                App(HandleKeyEvent(key!("esc"))),
                Expect(CompletionDropdownIsOpen(false)),
                App(HandleKeyEvent(key!("n"))),
                Expect(CompletionDropdownIsOpen(false)),
            ])
        })
    }

    #[test]
    fn enter_normal_mode_should_close_info() -> anyhow::Result<()> {
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent("".to_string())),
                SuggestiveEditor(CompletionFilter(SuggestiveEditorFilter::CurrentWord)),
                Editor(EnterInsertMode(Direction::Start)),
                App(ShowEditorInfo(Info::default())),
                Expect(EditorInfoOpen(true)),
                Expect(CurrentPath(s.main_rs())),
                App(HandleKeyEvent(key!("esc"))),
                Expect(EditorInfoOpen(false)),
            ])
        })
    }

    #[test]
    fn receiving_multiple_completion_should_not_increase_dropdown_infos_count(
    ) -> Result<(), anyhow::Error> {
        let completion = Completion {
            trigger_characters: vec![".".to_string()],
            items: [CompletionItem::from_label("hello".to_string())
                .set_documentation(Some(Documentation::new("This is a doc")))]
            .into_iter()
            .map(|item| item.into())
            .collect(),
        };
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent("".to_string())),
                SuggestiveEditor(CompletionFilter(SuggestiveEditorFilter::CurrentWord)),
                Editor(EnterInsertMode(Direction::Start)),
                Expect(DropdownInfosCount(0)),
                SuggestiveEditor(Completion(completion.clone())),
                Expect(DropdownInfosCount(1)),
                SuggestiveEditor(Completion(completion.clone())),
                Expect(DropdownInfosCount(1)),
            ])
        })
    }

    #[test]
    fn filter_with_current_word() -> Result<(), anyhow::Error> {
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent("".to_string())),
                SuggestiveEditor(CompletionFilter(SuggestiveEditorFilter::CurrentWord)),
                Editor(EnterInsertMode(Direction::Start)),
                SuggestiveEditor(Completion(dummy_completion())),
                App(HandleKeyEvents(keys!("p a").to_vec())),
                Expect(CompletionDropdownIsOpen(true)),
                Expect(CompletionDropdownContent(" Patrick")),
                App(HandleKeyEvents(keys!("space s").to_vec())),
                // Expect the completion dropdown to be open,
                // and the dropdown items to be filtered by the current word, 's'
                Expect(CompletionDropdownContent(" Spongebob\n Squidward")),
            ])
        })
    }

    #[test]
    fn setting_completion_when_not_in_insert_mode() -> Result<(), anyhow::Error> {
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent("".to_string())),
                SuggestiveEditor(CompletionFilter(SuggestiveEditorFilter::CurrentWord)),
                Editor(EnterNormalMode),
                // Pretend that the LSP server returned a completion
                SuggestiveEditor(Completion(dummy_completion())),
                // Expect the completion dropdown to not be opened,
                // since the editor is not in insert mode
                Expect(CompletionDropdownIsOpen(false)),
                Expect(CurrentSelectionMode(SelectionMode::Line)),
                Editor(MoveSelection(crate::components::editor::Movement::Right)),
                Expect(CompletionDropdownIsOpen(false)),
            ])
        })
    }

    #[test]
    fn completion_with_emoji() -> anyhow::Result<()> {
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent("".to_string())),
                SuggestiveEditor(CompletionFilter(SuggestiveEditorFilter::CurrentWord)),
                Editor(EnterInsertMode(Direction::Start)),
                // Pretend that the LSP server returned a completion
                // That is without edit, but contains `kind`, which means it has emoji
                SuggestiveEditor(Completion(Completion {
                    items: [CompletionItem {
                        label: "Spongebob".to_string(),
                        edit: None,
                        documentation: None,
                        sort_text: None,
                        insert_text: None,
                        kind: Some(CompletionItemKind::FUNCTION),
                        detail: None,
                        completion_item: Default::default(),
                    }]
                    .into_iter()
                    .map(|item| item.into())
                    .collect(),
                    trigger_characters: Vec::new(),
                })),
                App(HandleKeyEvent(key!("s"))),
                Expect(CompletionDropdownContent("💥 Spongebob")),
                App(HandleKeyEvent(key!("tab"))),
                // Expect the content of the buffer to be applied with the new edit,
                // resulting in 'Spongebob', and does not contain emoji
                Expect(CurrentComponentContent("Spongebob")),
            ])
        })
    }

    #[test]
    fn hide_dropdown_when_no_matching_candidates() -> Result<(), anyhow::Error> {
        execute_test(|s| {
            Box::new([
                App(OpenFile {
                    path: s.main_rs(),
                    owner: BufferOwner::User,
                    focus: true,
                }),
                Editor(SetContent("".to_string())),
                Editor(EnterInsertMode(Direction::Start)),
                SuggestiveEditor(CompletionFilter(SuggestiveEditorFilter::CurrentWord)),
                // Pretend that the LSP server returned a completion
                SuggestiveEditor(Completion(dummy_completion())),
                // Expect the completion dropdown to be open,
                Expect(CompletionDropdownContent(
                    " Patrick\n Spongebob\n Squidward",
                )),
                // Type in 'zz'
                App(HandleKeyEvents(keys!("z z").to_vec())),
                // Expect the dropdown is closed, because there's no matching candidates
                Expect(CompletionDropdownIsOpen(false)),
                SuggestiveEditor(Completion(dummy_completion())),
                Expect(CompletionDropdownIsOpen(false)),
            ])
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub(crate) struct Info {
    title: String,
    content: String,
    decorations: Vec<Decoration>,
}
impl Info {
    pub(crate) fn new(title: String, content: String) -> Info {
        Info {
            title,
            content,
            decorations: Vec::new(),
        }
    }

    pub(crate) fn content(&self) -> &String {
        &self.content
    }

    pub(crate) fn decorations(&self) -> &Vec<Decoration> {
        &self.decorations
    }

    pub(crate) fn set_decorations(self, decorations: Vec<Decoration>) -> Info {
        Info {
            decorations,
            ..self
        }
    }

    pub(crate) fn join(self, other: Info) -> Info {
        let separator = "=".repeat(10).to_string();
        let content = format!("{}\n{}\n{}", self.content, separator, other.content);
        let other_decorations = other
            .decorations
            .into_iter()
            .map(|decoration| decoration.increase_byte(separator.len() + 2))
            .collect_vec();
        let decorations = self
            .decorations
            .into_iter()
            .chain(other_decorations)
            .collect_vec();
        Info {
            title: self.title.clone(),
            content,
            decorations,
        }
    }

    pub(crate) fn title(&self) -> String {
        self.title.clone()
    }

    pub(crate) fn display(&self) -> String {
        format!("{}\n\n{}", self.title(), self.content())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Decoration {
    selection_range: SelectionRange,
    style_key: StyleKey,
    adjustments: Vec<Adjustment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Adjustment {
    IncreaseByte(usize),
}

impl Decoration {
    fn increase_byte(mut self, len: usize) -> Decoration {
        self.adjustments.push(Adjustment::IncreaseByte(len));
        self
    }

    pub(crate) fn selection_range(&self) -> &SelectionRange {
        &self.selection_range
    }

    pub(crate) fn style_key(&self) -> &StyleKey {
        &self.style_key
    }

    pub(crate) fn new(selection_range: SelectionRange, style_key: StyleKey) -> Decoration {
        Decoration {
            selection_range,
            style_key,
            adjustments: Default::default(),
        }
    }

    pub(crate) fn move_left(self, count: usize) -> Decoration {
        Decoration {
            selection_range: self.selection_range.move_left(count),
            ..self
        }
    }
}

pub(crate) fn completion_item_keymaps(context: &Context) -> Keymaps {
    Keymaps::new(&[
        Keymap::new_extended(
            context
                .keyboard_layout_kind()
                .get_insert_key(&Meaning::ScrlD),
            Direction::End.format_action("Comp"),
            "Next Completion Item".to_string(),
            Dispatch::MoveToCompletionItem(Direction::End),
        ),
        Keymap::new_extended(
            context
                .keyboard_layout_kind()
                .get_insert_key(&Meaning::ScrlU),
            Direction::Start.format_action("Comp"),
            "Previous Completion Item".to_string(),
            Dispatch::MoveToCompletionItem(Direction::Start),
        ),
        Keymap::new_extended(
            context
                .keyboard_layout_kind()
                .get_insert_key(&Meaning::MrkFN),
            "Select Comp".to_string(),
            "Select Completion Item".to_string(),
            Dispatch::SelectCompletionItem,
        ),
    ])
}
