/// NOTE: all test cases that involves the clipboard should not be run in parallel
///   otherwise the the test suite will fail because multiple tests are trying to
///   access the clipboard at the same time.
#[cfg(test)]
mod test_app {
    use itertools::Itertools;
    use my_proc_macros::key;
    use pretty_assertions::assert_eq;
    use serial_test::serial;

    use std::sync::{Arc, Mutex};
    use Dispatch::*;
    use DispatchEditor::*;
    use Movement::*;
    use SelectionMode::*;

    use shared::canonicalized_path::CanonicalizedPath;

    use crate::{
        app::{
            App, Dimension, Dispatch, GlobalSearchConfigUpdate, GlobalSearchFilterGlob,
            LocalSearchConfigUpdate, Scope,
        },
        components::{
            component::ComponentId,
            editor::{Direction, DispatchEditor, Movement},
            suggestive_editor::Info,
        },
        context::{GlobalMode, LocalSearchConfigMode},
        frontend::mock::MockFrontend,
        integration_test::integration_test::TestRunner,
        list::grep::RegexConfig,
        lsp::{process::LspNotification, signature_help::SignatureInformation},
        position::Position,
        quickfix_list::{Location, QuickfixListItem},
        selection::SelectionMode,
        selection_mode::inside::InsideKind,
    };

    #[derive(Clone)]
    enum Step {
        App(Dispatch),
        Expect(ExpectKind),
        Editor(DispatchEditor),
    }

    #[derive(Clone)]
    enum ExpectKind {
        CurrentFileContent(&'static str),
        FileContentEqual(CanonicalizedPath, CanonicalizedPath),
        CurrentSelectedTexts(&'static [&'static str]),
    }

    use ExpectKind::*;
    use Step::*;
    struct State {
        main_rs: CanonicalizedPath,
        foo_rs: CanonicalizedPath,
    }
    impl State {
        fn main_rs(&self) -> CanonicalizedPath {
            self.main_rs.clone()
        }

        fn foo_rs(&self) -> CanonicalizedPath {
            self.foo_rs.clone()
        }
    }

    fn execute_test(callback: impl Fn(State) -> Vec<Step>) -> anyhow::Result<()> {
        run_test(|mut app, temp_dir| {
            let steps = callback(State {
                main_rs: temp_dir.join("src/main.rs").unwrap(),
                foo_rs: temp_dir.join("src/foo.rs").unwrap(),
            });
            for step in steps {
                match step {
                    Step::App(dispatch) => app.handle_dispatch(dispatch.to_owned())?,
                    Step::Expect(expect) => match expect {
                        CurrentFileContent(expected_content) => {
                            assert_eq!(app.get_current_file_content(), expected_content.to_owned())
                        }
                        FileContentEqual(left, right) => {
                            assert_eq!(app.get_file_content(&left), app.get_file_content(&right))
                        }
                        CurrentSelectedTexts(selected_texts) => {
                            assert_eq!(app.get_current_selected_texts().1, selected_texts)
                        }
                    },
                    Editor(dispatch) => app.handle_dispatch_editor(dispatch)?,
                }
            }
            Ok(())
        })
    }

    fn run_test(
        callback: impl Fn(App<MockFrontend>, CanonicalizedPath) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        TestRunner::run(|temp_dir| {
            let mock_frontend = Arc::new(Mutex::new(MockFrontend::new()));
            let mut app = App::new(mock_frontend, temp_dir.clone())?;
            app.disable_lsp();
            callback(app, temp_dir)
        })
    }

    #[test]
    #[serial]
    fn copy_paste_from_different_file() -> anyhow::Result<()> {
        execute_test(|s| {
            [
                App(OpenFile(s.main_rs())),
                App(OpenFile(s.foo_rs())),
                Editor(SetSelectionMode(LineTrimmed)),
                Editor(SelectAll),
                Editor(Copy),
                App(OpenFile(s.foo_rs())),
                Editor(SetSelectionMode(LineTrimmed)),
                Editor(SelectAll),
                Editor(Copy),
                App(OpenFile(s.main_rs())),
                Editor(SelectAll),
                Editor(Paste),
                Expect(FileContentEqual(s.main_rs, s.foo_rs)),
            ]
            .to_vec()
        })
    }

    #[test]
    #[serial]
    fn copy_replace() -> anyhow::Result<()> {
        execute_test(|s| {
            [
                App(OpenFile(s.main_rs())),
                Editor(SetContent("fn main() { let x = 1; }".to_string())),
                Editor(SetSelectionMode(SelectionMode::BottomNode)),
                Editor(Copy),
                Editor(MoveSelection(Movement::Next)),
                Editor(ReplaceSelectionWithCopiedText),
                Expect(CurrentFileContent("fn fn() { let x = 1; }")),
                Editor(ReplaceSelectionWithCopiedText),
                Expect(CurrentSelectedTexts(&["main"])),
            ]
            .to_vec()
        })
    }

    #[test]
    #[serial]
    fn copy_paste() -> anyhow::Result<()> {
        execute_test(|s| {
            [
                App(OpenFile(s.main_rs())),
                Editor(SetContent("fn main() { let x = 1; }".to_string())),
                Editor(SetSelectionMode(SelectionMode::BottomNode)),
                Editor(Copy),
                Editor(MoveSelection(Movement::Next)),
                Editor(Paste),
                Expect(CurrentFileContent("fn fn() { let x = 1; }")),
                Expect(CurrentSelectedTexts(&[""])),
                Editor(MoveSelection(Next)),
                Editor(Paste),
                Expect(CurrentFileContent("fn fn(fn { let x = 1; }")),
            ]
            .to_vec()
        })
    }

    #[test]
    #[serial]
    fn cut_paste() -> anyhow::Result<()> {
        execute_test(|s| {
            [
                App(OpenFile(s.main_rs())),
                Editor(SetContent("fn main() { let x = 1; }".to_string())),
                Editor(SetSelectionMode(BottomNode)),
                Editor(Cut),
                Editor(EnterNormalMode),
                Expect(CurrentFileContent(" main() { let x = 1; }")),
                Editor(MoveSelection(Current)),
                Expect(CurrentSelectedTexts(&["main"])),
                Editor(Paste),
                Expect(CurrentFileContent(" fn() { let x = 1; }")),
            ]
            .to_vec()
        })
    }

    #[test]
    #[serial]
    fn highlight_mode_cut() -> anyhow::Result<()> {
        execute_test(|s| {
            [
                App(OpenFile(s.main_rs())),
                Editor(SetContent(
                    "fn f(){ let x = S(a); let y = S(b); }".to_string(),
                )),
                Editor(SetSelectionMode(BottomNode)),
                Editor(ToggleHighlightMode),
                Editor(MoveSelection(Next)),
                Editor(MoveSelection(Next)),
                Editor(MoveSelection(Next)),
                Expect(CurrentSelectedTexts(&["fn f()"])),
                Editor(Cut),
                Expect(CurrentFileContent("{ let x = S(a); let y = S(b); }")),
                Editor(Paste),
                Expect(CurrentFileContent("fn f(){ let x = S(a); let y = S(b); }")),
            ]
            .to_vec()
        })
    }

    #[test]
    #[serial]
    fn highlight_mode_copy() -> anyhow::Result<()> {
        execute_test(|s| {
            [
                App(OpenFile(s.main_rs())),
                Editor(SetContent(
                    "fn f(){ let x = S(a); let y = S(b); }".to_string(),
                )),
                Editor(SetSelectionMode(SelectionMode::BottomNode)),
                Editor(ToggleHighlightMode),
                Editor(MoveSelection(Movement::Next)),
                Editor(MoveSelection(Movement::Next)),
                Editor(MoveSelection(Movement::Next)),
                Expect(CurrentSelectedTexts(&["fn f()"])),
                Editor(Copy),
                Editor(MoveSelection(Next)),
                Expect(CurrentSelectedTexts(&["{"])),
                Editor(Paste),
                Expect(CurrentFileContent(
                    "fn f()fn f() let x = S(a); let y = S(b); }",
                )),
            ]
            .to_vec()
        })
    }

    #[test]
    #[serial]
    fn highlight_mode_replace() -> anyhow::Result<()> {
        execute_test(|s| {
            [
                App(OpenFile(s.main_rs())),
                Editor(SetContent(
                    "fn f(){ let x = S(a); let y = S(b); }".to_string(),
                )),
                Editor(SetSelectionMode(SelectionMode::BottomNode)),
                Editor(ToggleHighlightMode),
                Editor(MoveSelection(Movement::Next)),
                Editor(MoveSelection(Movement::Next)),
                Editor(MoveSelection(Movement::Next)),
                Expect(CurrentSelectedTexts(&["fn f()"])),
                Editor(Copy),
                Editor(MatchLiteral("{".to_string())),
                Editor(SetSelectionMode(SelectionMode::TopNode)),
                Expect(CurrentSelectedTexts(&["{ let x = S(a); let y = S(b); }"])),
                Editor(ReplaceSelectionWithCopiedText),
                Expect(CurrentFileContent("fn f()fn f()")),
            ]
            .to_vec()
        })
    }

    #[test]
    #[serial]
    fn highlight_mode_paste() -> anyhow::Result<()> {
        execute_test(|s| {
            [
                App(OpenFile(s.main_rs())),
                Editor(SetContent(
                    "fn f(){ let x = S(a); let y = S(b); }".to_string(),
                )),
                Editor(SetSelectionMode(SelectionMode::BottomNode)),
                Editor(Copy),
                Expect(CurrentSelectedTexts(&["fn"])),
                Editor(ToggleHighlightMode),
                Editor(MoveSelection(Next)),
                Editor(MoveSelection(Next)),
                Editor(MoveSelection(Next)),
                Expect(CurrentSelectedTexts(&["fn f()"])),
                Editor(Paste),
                Expect(CurrentFileContent("fn{ let x = S(a); let y = S(b); }")),
            ]
            .to_vec()
        })
    }

    #[test]
    #[serial]
    fn multi_paste() -> anyhow::Result<()> {
        run_test(|mut app, temp_dir| {
            let path_main = temp_dir.join("src/main.rs")?;
            app.handle_dispatch(OpenFile(path_main.clone()))?;
            app.handle_dispatch_editors(&[
                SetContent("fn f(){ let x = S(spongebob_squarepants); let y = S(b); }".to_string()),
                MatchLiteral("let x = S(spongebob_squarepants);".to_owned()),
                SetSelectionMode(SelectionMode::SyntaxTree),
            ])?;
            app.handle_dispatch_editors(&[
                CursorAddToAllSelections,
                MoveSelection(Movement::Down),
                MoveSelection(Movement::Next),
            ])?;
            assert_eq!(
                app.get_selected_texts(&path_main),
                vec!["S(spongebob_squarepants)", "S(b)"]
            );

            app.handle_dispatch_editors(&[
                Cut,
                EnterInsertMode(Direction::Start),
                Insert("Some(".to_owned()),
                Paste,
                Insert(")".to_owned()),
            ])?;

            assert_eq!(
                app.get_file_content(&path_main),
                "fn f(){ let x = Some(S(spongebob_squarepants)); let y = Some(S(b)); }"
            );

            app.handle_dispatch_editors(&[CursorKeepPrimaryOnly])?;
            app.handle_dispatch(SetClipboardContent(".hello".to_owned()))?;
            app.handle_dispatches(
                [
                    DispatchEditor(CursorKeepPrimaryOnly),
                    SetClipboardContent(".hello".to_owned()),
                    DispatchEditor(Paste),
                ]
                .to_vec(),
            )?;

            assert_eq!(
                app.get_file_content(&path_main),
                "fn f(){ let x = Some(S(spongebob_squarepants).hello; let y = Some(S(b)); }"
            );

            Ok(())
        })
    }

    #[test]
    fn esc_should_close_signature_help() -> anyhow::Result<()> {
        run_test(|mut app, temp_dir| {
            let path_main = temp_dir.join("src/main.rs")?;
            app.open_file(&path_main, true)?;

            assert_eq!(app.components().len(), 1);

            app.handle_dispatch_editors(&[
                SetContent("fn f(){ let x = S(a); let y = S(b); }".to_string()),
                SetSelectionMode(SelectionMode::BottomNode),
                EnterInsertMode(Direction::End),
            ])?;

            let component_id = app.components()[0].borrow().id();
            app.handle_lsp_notification(LspNotification::SignatureHelp(
                crate::lsp::process::ResponseContext {
                    component_id,
                    scope: None,
                    description: None,
                },
                Some(crate::lsp::signature_help::SignatureHelp {
                    signatures: [SignatureInformation {
                        label: "Signature Help".to_string(),
                        documentation: Some(crate::lsp::documentation::Documentation {
                            content: "spongebob".to_string(),
                        }),
                        active_parameter_byte_range: None,
                    }]
                    .to_vec(),
                }),
            ))?;
            assert_eq!(app.components().len(), 2);

            app.handle_dispatch(HandleKeyEvent(key!("esc")))?;
            assert_eq!(app.components().len(), 1);

            Ok(())
        })
    }

    #[test]
    pub fn repo_git_hunks() -> Result<(), anyhow::Error> {
        run_test(|mut app, temp_dir| {
            let path_main = temp_dir.join("src/main.rs")?;
            let path_foo = temp_dir.join("src/foo.rs")?;
            let path_new_file = temp_dir.join_as_path_buf("new_file.md");

            app.handle_dispatches(
                [
                    // Delete the first line of main.rs
                    OpenFile(path_main.clone()),
                    DispatchEditor(SetSelectionMode(SelectionMode::LineTrimmed)),
                    DispatchEditor(Kill),
                    // Insert a comment at the first line of foo.rs
                    OpenFile(path_foo.clone()),
                    DispatchEditor(Insert("// Hello".to_string())),
                    // Save the files,
                    SaveAll,
                    // Add a new file
                    AddPath(path_new_file.clone()),
                    // Get the repo hunks
                    GetRepoGitHunks,
                ]
                .to_vec(),
            )?;

            fn strs_to_strings(strs: &[&str]) -> Option<Info> {
                Some(Info::new(
                    strs.iter().map(|s| s.to_string()).join("\n").to_string(),
                ))
            }

            let expected_quickfixes = [
                QuickfixListItem::new(
                    Location {
                        path: path_new_file.try_into()?,
                        range: Position { line: 0, column: 0 }..Position { line: 0, column: 0 },
                    },
                    strs_to_strings(&["[This file is untracked by Git]"]),
                ),
                QuickfixListItem::new(
                    Location {
                        path: path_foo,
                        range: Position { line: 0, column: 0 }..Position { line: 1, column: 0 },
                    },
                    strs_to_strings(&["pub struct Foo {", "// Hellopub struct Foo {"]),
                ),
                QuickfixListItem::new(
                    Location {
                        path: path_main,
                        range: Position { line: 0, column: 0 }..Position { line: 0, column: 0 },
                    },
                    strs_to_strings(&["mod foo;"]),
                ),
            ];
            let actual_quickfixes = app
                .get_quickfixes()
                .into_iter()
                .map(|quickfix| {
                    let info = quickfix
                        .info()
                        .as_ref()
                        .map(|info| info.clone().set_decorations(Vec::new()));
                    quickfix.set_info(info)
                })
                .collect_vec();
            assert_eq!(actual_quickfixes, expected_quickfixes);

            Ok(())
        })
    }

    #[test]
    pub fn non_git_ignored_files() -> Result<(), anyhow::Error> {
        run_test(|mut app, temp_dir| {
            let path_git_ignore = temp_dir.join(".gitignore")?;

            app.handle_dispatches(
                [
                    // Ignore *.txt files
                    OpenFile(path_git_ignore.clone()),
                    DispatchEditor(Insert("*.txt\n".to_string())),
                    SaveAll,
                    // Add new txt file
                    AddPath(temp_dir.join_as_path_buf("temp.txt")),
                    // Add a new Rust file
                    AddPath(temp_dir.join_as_path_buf("src/rust.rs")),
                ]
                .to_vec(),
            )?;

            let paths = crate::git::GitRepo::try_from(&temp_dir)?.non_git_ignored_files()?;

            // Expect all the paths are files, not directory for example
            assert!(paths.iter().all(|file| file.is_file()));

            let paths = paths
                .into_iter()
                .flat_map(|path| path.display_relative_to(&temp_dir))
                .collect_vec();

            // Expect "temp.txt" is not in the list, since it is git-ignored
            assert!(!paths.contains(&"temp.txt".to_string()));

            // Expect the unstaged file "src/rust.rs" is in the list
            assert!(paths.contains(&"src/rust.rs".to_string()));

            // Expect the staged file "main.rs" is in the list
            assert!(paths.contains(&"src/main.rs".to_string()));

            Ok(())
        })
    }

    #[test]
    fn align_view_bottom_with_outbound_parent_lines() -> anyhow::Result<()> {
        run_test(|mut app, temp_dir| {
            let path_main = temp_dir.join_as_path_buf("src/main.rs");

            app.handle_dispatches(
                [
                    Dispatch::SetGlobalTitle("[GLOBAL TITLE]".to_string()),
                    OpenFile(path_main.try_into()?),
                    TerminalDimensionChanged(Dimension {
                        width: 200,
                        height: 6,
                    }),
                ]
                .to_vec(),
            )?;
            app.handle_dispatch_editors(&[
                SetSelectionMode(SelectionMode::LineTrimmed),
                SelectAll,
                Kill,
                Insert(
                    "
fn first () {
  second();
  third();
  fourth(); // this line is long
  fifth();
}"
                    .trim()
                    .to_string(),
                ),
                DispatchEditor::MatchLiteral("fifth()".to_string()),
                AlignViewTop,
            ])?;

            let result = app.get_grid()?;
            assert_eq!(
                result.to_string(),
                "
src/main.rs 🦀
1│fn first () {
5│  █ifth();
6│}

[GLOBAL TITLE]
"
                .trim()
            );

            app.handle_dispatch_editors(&[AlignViewBottom])?;

            let result = app.get_grid()?;
            assert_eq!(
                result.to_string(),
                "
src/main.rs 🦀
1│fn first () {
3│  third();
4│  fourth(); // this line is long
5│  █ifth();
[GLOBAL TITLE]
"
                .trim()
            );

            // Resize the terminal dimension sucht that the fourth line will be wrapped
            app.handle_dispatches(
                [
                    TerminalDimensionChanged(Dimension {
                        width: 20,
                        height: 6,
                    }),
                    DispatchEditor(AlignViewBottom),
                ]
                .to_vec(),
            )?;

            let result = app.get_grid()?;
            assert_eq!(
                result.to_string(),
                "
src/main.rs 🦀
1│fn first () {
4│  fourth(); //
↪│this line is long
5│  █ifth();
[GLOBAL TITLE]
"
                .trim()
            );
            Ok(())
        })
    }

    #[test]
    fn selection_history_contiguous() -> Result<(), anyhow::Error> {
        run_test(|mut app, temp_dir| {
            let file = |filename: &str| -> anyhow::Result<CanonicalizedPath> {
                temp_dir.join_as_path_buf(filename).try_into()
            };
            let open =
                |filename: &str| -> anyhow::Result<Dispatch> { Ok(OpenFile(file(filename)?)) };

            app.handle_dispatch(open("src/main.rs")?)?;
            let main_rs = &file("src/main.rs")?;

            app.handle_dispatch_editors(&[SetSelectionMode(SelectionMode::LineTrimmed)])?;
            assert_eq!(app.get_selected_texts(main_rs), ["mod foo;"]);

            app.handle_dispatch_editors(&[SetSelectionMode(SelectionMode::Character)])?;
            assert_eq!(app.get_selected_texts(main_rs), ["m"]);

            app.handle_dispatch(Dispatch::GoToPreviousSelection)?;
            assert_eq!(app.get_selected_texts(main_rs), ["mod foo;"]);

            app.handle_dispatch(Dispatch::GoToNextSelection)?;
            assert_eq!(app.get_selected_texts(main_rs), ["m"]);

            let foo_rs = file("src/foo.rs")?;
            app.handle_dispatch(Dispatch::GotoLocation(Location {
                path: foo_rs.clone(),
                range: Position::new(0, 0)..Position::new(0, 4),
            }))?;
            assert_eq!(
                app.get_current_selected_texts(),
                (foo_rs, ["pub ".to_string()].to_vec())
            );

            app.handle_dispatch(Dispatch::GoToPreviousSelection)?;
            assert_eq!(
                app.get_current_selected_texts(),
                (main_rs.clone(), ["m".to_string()].to_vec())
            );

            Ok(())
        })
    }

    #[test]
    fn selection_history_file() -> Result<(), anyhow::Error> {
        run_test(|mut app, temp_dir| {
            let file = |filename: &str| -> anyhow::Result<CanonicalizedPath> {
                temp_dir.join_as_path_buf(filename).try_into()
            };
            let open =
                |filename: &str| -> anyhow::Result<Dispatch> { Ok(OpenFile(file(filename)?)) };

            app.handle_dispatches(
                [
                    open("src/main.rs")?,
                    open("src/foo.rs")?,
                    open(".gitignore")?,
                    open("Cargo.toml")?,
                    // Move some selection to test that this movement ignore movement within the same file
                    DispatchEditor(SetSelectionMode(SelectionMode::LineTrimmed)),
                    DispatchEditor(MoveSelection(Movement::Next)),
                    // Open "Cargo.toml" again to test that the navigation tree does not take duplicated entry
                    open("Cargo.toml")?,
                ]
                .to_vec(),
            )?;

            assert_eq!(app.get_current_file_path(), Some(file("Cargo.toml")?));
            app.handle_dispatches(
                [SetGlobalMode(Some(GlobalMode::SelectionHistoryFile))].to_vec(),
            )?;
            app.handle_dispatch_editors(&[MoveSelection(Movement::Previous)])?;
            assert_eq!(app.get_current_file_path(), Some(file(".gitignore")?));

            app.handle_dispatch_editors(&[MoveSelection(Movement::Previous)])?;
            assert_eq!(app.get_current_file_path(), Some(file("src/foo.rs")?));

            // Test Movement::Next to src/foo.rs where no selection has been moved in src/foo.rs
            app.handle_dispatch_editors(&[
                MoveSelection(Movement::Previous),
                MoveSelection(Movement::Next),
            ])?;
            assert_eq!(app.get_current_file_path(), Some(file("src/foo.rs")?));

            app.handle_dispatches(
                [
                    // After moving back, open "src/foo.rs" again
                    // This is to make sure that "src/foo.rs" will not be
                    // added as a new entry
                    open("src/foo.rs")?,
                    open("Cargo.lock")?,
                    // Move some selection to test that the modified selection set is preserved when going to the next FileSelectionSet in the history
                    DispatchEditor(SetSelectionMode(SelectionMode::LineTrimmed)),
                    DispatchEditor(MoveSelection(Movement::Next)),
                    SetGlobalMode(Some(GlobalMode::SelectionHistoryFile)),
                ]
                .to_vec(),
            )?;
            assert_eq!(app.get_current_file_path(), Some(file("Cargo.lock")?));
            let cargo_lock_selection_set = app.get_current_selection_set();

            app.handle_dispatch_editors(&[MoveSelection(Movement::Previous)])?;
            assert_eq!(app.get_current_file_path(), Some(file("src/foo.rs")?));
            app.handle_dispatch_editors(&[MoveSelection(Movement::Next)])?;
            assert_eq!(app.get_current_file_path(), Some(file("Cargo.lock")?));
            assert_eq!(app.get_current_selection_set(), cargo_lock_selection_set);

            app.handle_dispatch(Dispatch::HandleKeyEvent(key!("esc")))?;
            assert_eq!(app.context().mode(), None);

            Ok(())
        })
    }

    #[test]
    fn global_bookmarks() -> Result<(), anyhow::Error> {
        run_test(|mut app, temp_dir| {
            let file = |filename: &str| -> anyhow::Result<CanonicalizedPath> {
                temp_dir.join_as_path_buf(filename).try_into()
            };
            let open =
                |filename: &str| -> anyhow::Result<Dispatch> { Ok(OpenFile(file(filename)?)) };

            app.handle_dispatches(
                [
                    open("src/main.rs")?,
                    DispatchEditor(SetSelectionMode(SelectionMode::Word)),
                    DispatchEditor(ToggleBookmark),
                    open("src/foo.rs")?,
                    DispatchEditor(SetSelectionMode(SelectionMode::Word)),
                    DispatchEditor(ToggleBookmark),
                    SetQuickfixList(crate::quickfix_list::QuickfixListType::Bookmark),
                ]
                .to_vec(),
            )?;
            assert_eq!(
                app.get_quickfixes(),
                [
                    QuickfixListItem::new(
                        Location {
                            path: file("src/foo.rs")?,
                            range: Position { line: 0, column: 0 }..Position { line: 0, column: 3 },
                        },
                        None,
                    ),
                    QuickfixListItem::new(
                        Location {
                            path: file("src/main.rs")?,
                            range: Position { line: 0, column: 0 }..Position { line: 0, column: 3 },
                        },
                        None,
                    ),
                ],
            );

            Ok(())
        })
    }

    #[test]
    fn search_config_history() -> Result<(), anyhow::Error> {
        run_test(|mut app, _| {
            let owner_id = ComponentId::new();
            let update = |scope: Scope, update: LocalSearchConfigUpdate| -> Dispatch {
                UpdateLocalSearchConfig {
                    owner_id,
                    update,
                    scope,
                    show_legend: true,
                }
            };
            let update_global = |update: GlobalSearchConfigUpdate| -> Dispatch {
                UpdateGlobalSearchConfig { owner_id, update }
            };
            use GlobalSearchConfigUpdate::*;
            use GlobalSearchFilterGlob::*;
            use LocalSearchConfigUpdate::*;
            use Scope::*;
            app.handle_dispatches(
                [
                    update(Local, SetSearch("L-Search1".to_string())),
                    update(Local, SetSearch("L-Search2".to_string())),
                    update(Local, SetSearch("L-Search1".to_string())),
                    update(Local, SetReplacement("L-Replacement1".to_string())),
                    update(Local, SetReplacement("L-Replacement2".to_string())),
                    update(Local, SetReplacement("L-Replacement1".to_string())),
                    update(Global, SetSearch("G-Search1".to_string())),
                    update(Global, SetSearch("G-Search2".to_string())),
                    update(Global, SetSearch("G-Search1".to_string())),
                    update(Global, SetReplacement("G-Replacement1".to_string())),
                    update(Global, SetReplacement("G-Replacement2".to_string())),
                    update(Global, SetReplacement("G-Replacement1".to_string())),
                    update_global(SetGlob(Exclude, "ExcludeGlob1".to_string())),
                    update_global(SetGlob(Exclude, "ExcludeGlob2".to_string())),
                    update_global(SetGlob(Exclude, "ExcludeGlob1".to_string())),
                    update_global(SetGlob(Include, "IncludeGlob1".to_string())),
                    update_global(SetGlob(Include, "IncludeGlob2".to_string())),
                    update_global(SetGlob(Include, "IncludeGlob1".to_string())),
                ]
                .to_vec(),
            )?;

            // Expect the histories are stored, where:
            // 1. There's no duplication
            // 2. The insertion order is up-to-date
            let context = app.context();
            let local = context.local_search_config();
            let global = context.global_search_config().local_config();
            assert_eq!(local.searches(), ["L-Search2", "L-Search1"]);
            assert_eq!(local.replacements(), ["L-Replacement2", "L-Replacement1"]);
            assert_eq!(global.searches(), ["G-Search2", "G-Search1"]);
            assert_eq!(global.replacements(), ["G-Replacement2", "G-Replacement1"]);

            let global = context.global_search_config();
            assert_eq!(global.include_globs(), ["IncludeGlob2", "IncludeGlob1"]);
            assert_eq!(global.include_globs(), ["IncludeGlob2", "IncludeGlob1"]);
            assert_eq!(global.exclude_globs(), ["ExcludeGlob2", "ExcludeGlob1"]);
            assert_eq!(global.exclude_globs(), ["ExcludeGlob2", "ExcludeGlob1"]);

            Ok(())
        })
    }

    #[test]
    fn global_search_and_replace() -> Result<(), anyhow::Error> {
        run_test(|mut app, temp_dir| {
            let file = |filename: &str| -> anyhow::Result<CanonicalizedPath> {
                temp_dir.join_as_path_buf(filename).try_into()
            };
            let owner_id = ComponentId::new();
            let new_dispatch = |update: LocalSearchConfigUpdate| -> Dispatch {
                UpdateLocalSearchConfig {
                    owner_id,
                    update,
                    scope: Scope::Global,
                    show_legend: true,
                }
            };
            let main_rs = file("src/main.rs")?;
            let foo_rs = file("src/foo.rs")?;
            let main_rs_initial_content = main_rs.read()?;
            // Initiall, expect main.rs and foo.rs to contain the word "foo"
            assert!(main_rs_initial_content.contains("foo"));
            assert!(foo_rs.read()?.contains("foo"));

            // Replace "foo" with "haha" globally
            app.handle_dispatches(
                [
                    OpenFile(main_rs.clone()),
                    new_dispatch(LocalSearchConfigUpdate::SetMode(
                        LocalSearchConfigMode::Regex(RegexConfig {
                            escaped: true,
                            case_sensitive: false,
                            match_whole_word: false,
                        }),
                    )),
                    new_dispatch(LocalSearchConfigUpdate::SetSearch("foo".to_string())),
                    new_dispatch(LocalSearchConfigUpdate::SetReplacement("haha".to_string())),
                    Dispatch::Replace {
                        scope: Scope::Global,
                    },
                ]
                .to_vec(),
            )?;

            // Expect main.rs and foo.rs to not contain the word "foo"
            assert!(!main_rs.read()?.contains("foo"));
            assert!(!foo_rs.read()?.contains("foo"));

            // Expect main.rs and foo.rs to contain the word "haha"
            assert!(main_rs.read()?.contains("haha"));
            assert!(foo_rs.read()?.contains("haha"));

            // Expect the main.rs buffer to be updated as well
            assert_eq!(app.get_file_content(&main_rs), main_rs.read()?);

            // Apply undo to main_rs
            app.handle_dispatch_editors(&[DispatchEditor::Undo])?;

            // Expect the content of the main.rs buffer to be reverted
            assert_eq!(app.get_file_content(&main_rs), main_rs_initial_content);

            Ok(())
        })
    }

    #[test]
    /// Example: from "hello" -> hello
    fn raise_inside() -> anyhow::Result<()> {
        run_test(|mut app, temp_dir| {
            app.handle_dispatch(OpenFile(temp_dir.join("src/main.rs")?))?;
            app.handle_dispatch_editors(&[
                SetContent("fn main() { (a, b) }".to_string()),
                MatchLiteral("b".to_string()),
                SetSelectionMode(SelectionMode::Inside(InsideKind::Parentheses)),
            ])?;
            assert_eq!(app.get_current_selected_texts().1, &["a, b"]);
            app.handle_dispatch_editors(&[Raise])?;

            assert_eq!(app.get_current_file_content(), "fn main() { a, b }");
            Ok(())
        })
    }

    #[test]
    fn toggle_highlight_mode() -> anyhow::Result<()> {
        run_test(|mut app, temp_dir| {
            app.handle_dispatch(OpenFile(temp_dir.join("src/main.rs")?))?;
            app.handle_dispatch_editors(&[
                SetContent("fn f(){ let x = S(a); let y = S(b); }".to_string()),
                SetSelectionMode(BottomNode),
                ToggleHighlightMode,
                MoveSelection(Next),
                MoveSelection(Next),
            ])?;
            assert_eq!(app.get_current_selected_texts().1, vec!["fn f("]);

            // Toggle the second time should inverse the initial_range
            app.handle_dispatch_editors(&[ToggleHighlightMode, MoveSelection(Next)])?;
            assert_eq!(app.get_current_selected_texts().1, vec!["f("]);

            app.handle_dispatch_editors(&[Reset])?;

            assert_eq!(app.get_current_selected_texts().1, vec!["f"]);

            app.handle_dispatch_editors(&[MoveSelection(Next)])?;

            assert_eq!(app.get_current_selected_texts().1, vec!["("]);

            Ok(())
        })
    }

    #[test]
    /// Kill means delete until the next selection
    fn delete_should_kill_if_possible_1() -> anyhow::Result<()> {
        run_test(|mut app, temp_dir| {
            app.handle_dispatch(OpenFile(temp_dir.join("src/main.rs")?))?;
            app.handle_dispatch_editors(&[
                SetContent("fn main() {}".to_string()),
                SetSelectionMode(BottomNode),
                Kill,
            ])?;

            // Expect the text to be 'main() {}'
            assert_eq!(app.get_current_file_content(), "main() {}");

            // Expect the current selection is 'main'
            assert_eq!(app.get_current_selected_texts().1, vec!["main"]);

            Ok(())
        })
    }

    #[test]
    /// No gap between current and next selection
    fn delete_should_kill_if_possible_2() -> anyhow::Result<()> {
        run_test(|mut app, temp_dir| {
            app.handle_dispatches([OpenFile(temp_dir.join("src/main.rs")?)].to_vec())?;
            app.handle_dispatch_editors(&[
                SetContent("fn main() {}".to_string()),
                SetSelectionMode(Character),
                Kill,
            ])?;
            assert_eq!(app.get_current_file_content(), "n main() {}");

            // Expect the current selection is 'n'
            assert_eq!(app.get_current_selected_texts().1, vec!["n"]);
            Ok(())
        })
    }

    #[test]
    /// No next selection
    fn delete_should_kill_if_possible_3() -> anyhow::Result<()> {
        run_test(|mut app, temp_dir| {
            app.handle_dispatch(OpenFile(temp_dir.join("src/main.rs")?))?;
            app.handle_dispatch_editors(&[
                SetContent("fn main() {}".to_string()),
                SetSelectionMode(BottomNode),
                MoveSelection(Last),
                Kill,
            ])?;
            assert_eq!(app.get_current_file_content(), "fn main() {");

            Ok(())
        })
    }

    #[test]
    /// The selection mode is contiguous
    fn delete_should_kill_if_possible_4() -> anyhow::Result<()> {
        run_test(|mut app, temp_dir| {
            app.handle_dispatch(OpenFile(temp_dir.join("src/main.rs")?))?;
            app.handle_dispatch_editors(&[
                SetContent("fn main(a:A,b:B) {}".to_string()),
                MatchLiteral("a:A".to_string()),
                SetSelectionMode(SyntaxTree),
                Kill,
            ])?;
            assert_eq!(app.get_current_file_content(), "fn main(b:B) {}");

            // Expect the current selection is 'b:B'
            assert_eq!(app.get_current_selected_texts().1, vec!["b:B"]);

            Ok(())
        })
    }

    #[test]
    fn delete_should_not_kill_if_not_possible() -> anyhow::Result<()> {
        run_test(|mut app, temp_dir| {
            app.handle_dispatch(OpenFile(temp_dir.join("src/main.rs")?))?;
            app.handle_dispatch_editors(&[
                SetContent("fn maima() {}".to_string()),
                MatchLiteral("ma".to_string()),
                Kill,
            ])?;
            // Expect the text to be 'fn ima() {}'
            assert_eq!(app.get_current_file_content(), "fn ima() {}");

            // Expect the current selection is the character after "ma"
            assert_eq!(app.get_current_selected_texts().1, vec!["i"]);
            Ok(())
        })
    }

    #[test]
    fn toggle_untoggle_bookmark() -> anyhow::Result<()> {
        run_test(|mut app, temp_dir| {
            app.handle_dispatch(OpenFile(temp_dir.join("src/main.rs")?))?;
            app.handle_dispatch_editors(&[
                SetContent("foo bar spam".to_string()),
                SetSelectionMode(Word),
                ToggleBookmark,
                MoveSelection(Next),
                MoveSelection(Next),
                ToggleBookmark,
                SetSelectionMode(Bookmark),
                CursorAddToAllSelections,
            ])?;
            assert_eq!(app.get_current_selected_texts().1, ["foo", "spam"]);
            app.handle_dispatch_editors(&[CursorKeepPrimaryOnly])?;
            assert_eq!(app.get_current_selected_texts().1, ["spam"]);

            // Toggling the bookmark when selecting existing bookmark should
            app.handle_dispatch_editors(&[
                ToggleBookmark,
                MoveSelection(Current),
                CursorAddToAllSelections,
            ])?;
            assert_eq!(app.get_current_selected_texts().1, ["foo"]);
            Ok(())
        })
    }

    #[test]
    fn test_delete_word_backward_from_end_of_file() -> anyhow::Result<()> {
        run_test(|mut app, temp_dir| {
            app.handle_dispatch(OpenFile(temp_dir.join("src/main.rs")?))?;
            app.handle_dispatch_editors(&[
                SetContent("fn snake_case(camelCase: String) {}".to_string()),
                SetSelectionMode(LineTrimmed),
                // Go to the end of the file
                EnterInsertMode(Direction::End),
                DeleteWordBackward,
            ])?;
            assert_eq!(
                app.get_current_file_content(),
                "fn snake_case(camelCase: String) "
            );

            app.handle_dispatch_editors(&[DeleteWordBackward])?;
            assert_eq!(
                app.get_current_file_content(),
                "fn snake_case(camelCase: String"
            );

            app.handle_dispatch_editors(&[DeleteWordBackward])?;
            assert_eq!(app.get_current_file_content(), "fn snake_case(camelCase: ");

            Ok(())
        })
    }

    #[test]
    fn test_delete_word_backward_from_middle_of_file() -> anyhow::Result<()> {
        run_test(|mut app, temp_dir| {
            app.handle_dispatch(OpenFile(temp_dir.join("src/main.rs")?))?;
            app.handle_dispatch_editors(&[
                SetContent("fn snake_case(camelCase: String) {}".to_string()),
                SetSelectionMode(BottomNode),
                // Go to the middle of the file
                MoveSelection(Index(3)),
            ])?;
            assert_eq!(app.get_current_selected_texts().1, vec!["camelCase"]);

            app.handle_dispatch_editors(&[EnterInsertMode(Direction::End), DeleteWordBackward])?;

            assert_eq!(
                app.get_current_file_content(),
                "fn snake_case(camel: String) {}"
            );

            app.handle_dispatch_editors(&[DeleteWordBackward])?;
            assert_eq!(app.get_current_file_content(), "fn snake_case(: String) {}");

            app.handle_dispatch_editors(&[DeleteWordBackward])?;
            assert_eq!(app.get_current_file_content(), "fn snake_case: String) {}");

            app.handle_dispatch_editors(&[DeleteWordBackward])?;
            assert_eq!(app.get_current_file_content(), "fn snake_: String) {}");

            app.handle_dispatch_editors(&[DeleteWordBackward])?;
            assert_eq!(app.get_current_file_content(), "fn : String) {}");

            app.handle_dispatch_editors(&[DeleteWordBackward])?;
            assert_eq!(app.get_current_file_content(), ": String) {}");

            app.handle_dispatch_editors(&[DeleteWordBackward])?;
            assert_eq!(app.get_current_file_content(), ": String) {}");

            Ok(())
        })
    }
}
