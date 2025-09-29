use core::alloc;
use std::{
    collections::HashMap,
    hash::Hash,
    mem::{ManuallyDrop, MaybeUninit},
    ptr::slice_from_raw_parts,
};

use crossterm::event::{self, Event, KeyCode};
use decentralecs::{ColumnsApi, Entity, WithEntities, World};
use decentralecs_dynamic::DynamicTable;
use rand::Rng;
use ratatui::{
    DefaultTerminal, Frame,
    layout::{self, Constraint, Layout},
    style::{Color, Modifier, Style, Stylize, palette::tailwind},
    text::{Line, Text},
    widgets::{
        self, Block, HighlightSpacing, Paragraph, Row, ScrollbarState, Table, TableState, Widget,
    },
};
use tui_input::{Input, backend::crossterm::EventHandler};

mod replay_format;

// TODO:
// * Add new entities
// * Add components to entities
// * Support more complex datatypes than i16/String/bool (ADTs defined on disk as a config)
// * Validate the data written by the user
// * Persist data to disk and load it on startup

/// SAFETY: `T` must not contain `UnsafeCell` without going through indirection
unsafe fn uninit_slice_from_borrow<T: ?Sized>(data: &T) -> &[MaybeUninit<u8>] {
    let size = size_of_val(data);
    let ptr = slice_from_raw_parts(data as *const T as *const MaybeUninit<u8>, size);
    unsafe { &*ptr }
}

struct ReplayDB {
    world: World<'static>,
    labels: Vec<Label>,
    columns: HashMap<Label, DynamicTable>,
}

impl ReplayDB {
    fn new() -> Self {
        let labels = [
            Label {
                name: "Name".to_string(),
                data: LabelDataKind::Text,
            },
            Label {
                name: "800 Split".to_string(),
                data: LabelDataKind::Number,
            },
            Label {
                name: "PB".to_string(),
                data: LabelDataKind::Unit,
            },
        ];

        let mut world = World::new();

        let mut columns = HashMap::from([
            (
                labels[0].clone(),
                DynamicTable::new(&mut world, alloc::Layout::new::<String>()),
            ),
            (
                labels[1].clone(),
                DynamicTable::new(&mut world, alloc::Layout::new::<i16>()),
            ),
            (
                labels[2].clone(),
                DynamicTable::new(&mut world, alloc::Layout::new::<()>()),
            ),
        ]);

        let mut rng = rand::rng();
        for _ in 0..10 {
            let name: ManuallyDrop<String> = ManuallyDrop::new(
                (0..(rng.random_range(1..8)))
                    .map(|_| 'a')
                    .collect::<String>(),
            );
            let split: &i16 = &rng.random_range(-100..=182);
            let pb = rng.random();

            let mut builder = world.spawn();
            // FIXME: `insert` should probably not be a reference for `DynamicTable`. It doesn't imply
            // ownership semantics.
            builder
                .insert(columns.get_mut(&labels[0].clone()).unwrap(), unsafe {
                    uninit_slice_from_borrow::<ManuallyDrop<String>>(&name)
                })
                .insert(columns.get_mut(&labels[1].clone()).unwrap(), unsafe {
                    uninit_slice_from_borrow::<i16>(split)
                });

            if pb {
                builder.insert(columns.get_mut(&labels[2].clone()).unwrap(), unsafe {
                    uninit_slice_from_borrow(&())
                });
            }
        }

        Self {
            world,
            labels: labels.into(),
            columns,
        }
    }
}

#[derive(Eq, PartialEq, Hash, Debug, Clone)]
struct Label {
    name: String,
    data: LabelDataKind,
}

#[derive(Eq, PartialEq, Hash, Debug, Clone)]
enum LabelDataKind {
    Number,
    Text,
    Unit,
}

struct App {
    replay_db: ReplayDB,
    state: AppState,
}

enum AppState {
    ReplayDBViewer {
        table_state: TableState,
        scroll_state: ScrollbarState,
    },
    ReplayInfoEditor(ReplayInfoEditor),
}

struct ReplayInfoEditor {
    entity: Entity,
    focus: ReplayInfoEditorFocus,
    labels: Vec<LabelInput>,
}

#[derive(Copy, Clone, Debug)]
enum ReplayInfoEditorFocus {
    LabelData(usize),
    LabelRemove(usize),
    LabelAdd,
    SaveChanges,
}

impl ReplayInfoEditorFocus {
    #[must_use]
    fn next_focus(self, max_labels: usize, for_deletion: bool) -> Self {
        match self {
            ReplayInfoEditorFocus::LabelData(n) => ReplayInfoEditorFocus::LabelRemove(n),
            ReplayInfoEditorFocus::LabelRemove(n) => {
                if max_labels == n + 1 {
                    ReplayInfoEditorFocus::LabelAdd
                // If we're going to delete the previous focus then the index doesn't need to be incremented
                } else if for_deletion {
                    ReplayInfoEditorFocus::LabelData(n)
                } else {
                    ReplayInfoEditorFocus::LabelData(n + 1)
                }
            }
            ReplayInfoEditorFocus::LabelAdd => ReplayInfoEditorFocus::SaveChanges,
            ReplayInfoEditorFocus::SaveChanges => ReplayInfoEditorFocus::SaveChanges,
        }
    }

    #[must_use]
    fn prev_focus(self, max_labels: usize) -> Self {
        match self {
            ReplayInfoEditorFocus::LabelRemove(n) => ReplayInfoEditorFocus::LabelData(n),
            ReplayInfoEditorFocus::LabelData(n) => {
                if n == 0 {
                    ReplayInfoEditorFocus::LabelData(n)
                } else {
                    ReplayInfoEditorFocus::LabelRemove(n - 1)
                }
            }
            ReplayInfoEditorFocus::LabelAdd => {
                if max_labels >= 1 {
                    ReplayInfoEditorFocus::LabelRemove(max_labels - 1)
                } else {
                    ReplayInfoEditorFocus::LabelAdd
                }
            }
            ReplayInfoEditorFocus::SaveChanges => ReplayInfoEditorFocus::LabelAdd,
        }
    }
}

struct LabelInput {
    label: Label,
    data: Input,
}

impl ReplayInfoEditor {
    fn new(db: &ReplayDB, entity: Entity) -> Self {
        let labels = db
            .labels
            .iter()
            .flat_map(|label| {
                let data = db.columns[label].get_component(&db.world, entity)?;

                let existing_input = match label.data {
                    LabelDataKind::Number => {
                        let typed_data =
                            unsafe { *(data as *const [MaybeUninit<u8>] as *const i16) };
                        format!("{typed_data}")
                    }
                    LabelDataKind::Text => {
                        let typed_data =
                            unsafe { &*(data as *const [MaybeUninit<u8>] as *const String) };
                        typed_data.clone()
                    }
                    LabelDataKind::Unit => "".to_string(),
                };

                Some(LabelInput {
                    label: label.clone(),
                    data: Input::new(existing_input),
                })
            })
            .collect::<Vec<_>>();

        Self {
            entity,
            focus: if labels.len() > 0 {
                ReplayInfoEditorFocus::LabelData(0)
            } else {
                ReplayInfoEditorFocus::LabelAdd
            },
            labels,
        }
    }
}

fn main() {
    let mut terminal = ratatui::init();
    let app = App::new();
    app.run(&mut terminal);
    ratatui::restore();
}

impl App {
    fn new() -> Self {
        App {
            replay_db: ReplayDB::new(),
            state: AppState::ReplayDBViewer {
                table_state: TableState::default().with_selected(0),
                scroll_state: ScrollbarState::new(0),
            },
        }
    }

    fn run(mut self, terminal: &mut DefaultTerminal) {
        loop {
            terminal.draw(|frame| self.draw(frame)).unwrap();

            match &mut self.state {
                AppState::ReplayDBViewer {
                    table_state,
                    scroll_state,
                } => {
                    let event = event::read().unwrap();
                    if let Event::Key(key) = event {
                        match key.code {
                            KeyCode::Esc => return,
                            KeyCode::Up => self.prev_row(),
                            KeyCode::Down => self.next_row(),
                            KeyCode::Right => table_state.select_next_column(),
                            KeyCode::Left => table_state.select_previous_column(),
                            KeyCode::Char('e') => {
                                let selected_row = table_state.selected().unwrap();

                                let (_, selected_entity) = self
                                    .replay_db
                                    .world
                                    .join(WithEntities)
                                    .enumerate()
                                    .find(|(n, _)| n == &selected_row)
                                    .unwrap();

                                self.state = AppState::ReplayInfoEditor(ReplayInfoEditor::new(
                                    &self.replay_db,
                                    selected_entity,
                                ));
                            }
                            _ => (),
                        }
                    }
                }
                AppState::ReplayInfoEditor(ReplayInfoEditor {
                    entity,
                    focus,
                    labels,
                }) => {
                    let event = event::read().unwrap();
                    if let Event::Key(key) = event {
                        match key.code {
                            KeyCode::Esc => {
                                self.state = AppState::ReplayDBViewer {
                                    table_state: TableState::default().with_selected(0),
                                    scroll_state: ScrollbarState::new(0),
                                }
                            }
                            KeyCode::Up => *focus = focus.prev_focus(labels.len()),
                            KeyCode::Down | KeyCode::Tab => {
                                *focus = focus.next_focus(labels.len(), false)
                            }
                            KeyCode::Enter => match *focus {
                                ReplayInfoEditorFocus::LabelData(n) => {
                                    *focus = focus.next_focus(labels.len(), false);
                                }
                                ReplayInfoEditorFocus::LabelRemove(n) => {
                                    *focus = focus.next_focus(labels.len(), true);
                                    labels.remove(n);
                                }
                                ReplayInfoEditorFocus::LabelAdd => {
                                    *focus = focus.next_focus(labels.len(), false);
                                }
                                ReplayInfoEditorFocus::SaveChanges => {
                                    if labels.is_empty() {
                                        self.replay_db.world.despawn(*entity);
                                    } else {
                                        // FIXME: this is really slow lol. (but maybe doesn't matter?)
                                        for label in &self.replay_db.labels {
                                            let col =
                                                self.replay_db.columns.get_mut(label).unwrap();
                                            col.remove_component(
                                                &mut self.replay_db.world,
                                                *entity,
                                            );
                                        }

                                        for label in labels {
                                            let (n, s);

                                            let col = self
                                                .replay_db
                                                .columns
                                                .get_mut(&label.label)
                                                .unwrap();

                                            // FIXME: actually require the user written data is validated
                                            let typed_data = match label.label.data {
                                                LabelDataKind::Number => unsafe {
                                                    n = str::parse::<i16>(label.data.value())
                                                        .unwrap();
                                                    uninit_slice_from_borrow::<i16>(&n)
                                                },
                                                LabelDataKind::Text => unsafe {
                                                    s = ManuallyDrop::new(
                                                        label.data.value().to_string(),
                                                    );
                                                    uninit_slice_from_borrow::<ManuallyDrop<String>>(
                                                        &s,
                                                    )
                                                },
                                                LabelDataKind::Unit => unsafe {
                                                    uninit_slice_from_borrow(&())
                                                },
                                            };

                                            col.insert_component(
                                                &mut self.replay_db.world,
                                                *entity,
                                                typed_data,
                                            );
                                        }
                                    }

                                    self.state = AppState::ReplayDBViewer {
                                        table_state: TableState::default().with_selected(0),
                                        scroll_state: ScrollbarState::new(0),
                                    };
                                }
                            },
                            _ => match focus {
                                ReplayInfoEditorFocus::LabelData(n) => {
                                    _ = labels[*n].data.handle_event(&event);
                                }
                                ReplayInfoEditorFocus::SaveChanges
                                | ReplayInfoEditorFocus::LabelRemove(_)
                                | ReplayInfoEditorFocus::LabelAdd => (),
                            },
                        }
                    }
                }
            }
        }
    }

    fn next_row(&mut self) {
        let AppState::ReplayDBViewer { table_state, .. } = &mut self.state else {
            return;
        };

        let i = match table_state.selected() {
            Some(i) => {
                if i >= /* self.items.len() */ 10 - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        table_state.select(Some(i));
    }

    fn prev_row(&mut self) {
        let AppState::ReplayDBViewer { table_state, .. } = &mut self.state else {
            return;
        };

        let i = match table_state.selected() {
            Some(i) => {
                if i == 0 {
                    /* self.items.len() */
                    10 - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        table_state.select(Some(i));
    }

    fn draw(&mut self, frame: &mut Frame) {
        match &mut self.state {
            AppState::ReplayDBViewer {
                table_state,
                scroll_state,
            } => {
                let header_style = Style::default()
                    .fg(tailwind::SLATE.c200)
                    .bg(tailwind::BLUE.c900);
                let selected_row_style = Style::default()
                    .add_modifier(Modifier::REVERSED)
                    .fg(tailwind::BLUE.c400);
                let selected_col_style = Style::default().fg(tailwind::BLUE.c400);
                let selected_cell_style = Style::default()
                    .add_modifier(Modifier::REVERSED)
                    .fg(tailwind::BLUE.c600);

                let header = self
                    .replay_db
                    .labels
                    .iter()
                    .map(|label| &*label.name)
                    .into_iter()
                    .map(widgets::Cell::from)
                    .collect::<Row>()
                    .style(header_style)
                    .height(1);

                let rows = self
                    .replay_db
                    .world
                    .join(WithEntities)
                    .enumerate()
                    .map(|(i, e)| {
                        let color = match i % 2 {
                            0 => tailwind::SLATE.c950,
                            _ => tailwind::SLATE.c900,
                        };

                        let row_data = self.replay_db.labels.iter().map(|label| {
                            let raw_data = self
                                .replay_db
                                .columns
                                .get(label)
                                .unwrap()
                                .get_component(&self.replay_db.world, e);

                            let Some(raw_data) = raw_data else {
                                return "".to_string();
                            };

                            match label.data {
                                LabelDataKind::Number => {
                                    let typed_data = unsafe {
                                        *(raw_data as *const [MaybeUninit<u8>] as *const i16)
                                    };

                                    format!("{typed_data}")
                                }
                                LabelDataKind::Text => {
                                    let typed_data = unsafe {
                                        &*(raw_data as *const [MaybeUninit<u8>] as *const String)
                                    };

                                    typed_data.clone()
                                }
                                LabelDataKind::Unit => "X".to_string(),
                            }
                        });

                        row_data
                            .map(|content| {
                                widgets::Cell::from(Text::from(format!("\n{content}\n")))
                            })
                            .collect::<Row>()
                            .style(Style::new().fg(tailwind::SLATE.c200).bg(color))
                            .height(4)
                    });

                let bar = " █ ";
                let table = Table::new(
                    rows,
                    // FIXME: Properly track max width of columns
                    [Constraint::Min(10), Constraint::Min(10), Constraint::Min(9)],
                )
                .header(header)
                .row_highlight_style(selected_row_style)
                .column_highlight_style(selected_col_style)
                .cell_highlight_style(selected_cell_style)
                .highlight_symbol(Text::from(vec![
                    "".into(),
                    bar.into(),
                    bar.into(),
                    "".into(),
                ]))
                .bg(tailwind::SLATE.c950)
                .highlight_spacing(HighlightSpacing::Always);

                frame.render_stateful_widget(table, frame.area(), table_state);
            }

            AppState::ReplayInfoEditor(ReplayInfoEditor {
                entity: _,
                focus,
                labels,
            }) => {
                // meow
                let areas = layout::Layout::vertical(Constraint::from_lengths(
                    (0..(labels.len() * 2 + 2)).map(|_| 1),
                ))
                .split(frame.area());

                for (n, label) in labels.iter().enumerate() {
                    // Draw the label name + user input
                    let area = areas[n * 2];

                    let style = if let ReplayInfoEditorFocus::LabelData(n2) = focus
                        && *n2 == n
                    {
                        Color::Yellow.into()
                    } else {
                        Style::default()
                    };

                    let constraints = [
                        Constraint::Length(label.label.name.len() as u16 + 2),
                        Constraint::Fill(0),
                    ];
                    let [label_area, value_area] = Layout::horizontal(constraints).areas(area);

                    let line = Line::from_iter([&*label.label.name, ": "])
                        .bold()
                        .style(style);
                    frame.render_widget(line, label_area);
                    frame.render_widget(label.data.value(), value_area);

                    // Draw the delete label "button"
                    let area = areas[n * 2 + 1];
                    let style: Style = if let ReplayInfoEditorFocus::LabelRemove(n2) = focus
                        && *n2 == n
                    {
                        Color::Red.into()
                    } else {
                        Color::Black.into()
                    };
                    let line = Line::raw("Delete Label").style(style).bold();
                    frame.render_widget(line, area);
                }

                // Draw the add label "button"
                let area = areas[labels.len() * 2];
                let style: Style = if let ReplayInfoEditorFocus::LabelAdd = focus {
                    Color::Blue.into()
                } else {
                    Color::Black.into()
                };
                let line = Line::raw("Add Label").style(style).bold();
                frame.render_widget(line, area);

                // Draw the save changes "button"
                let area = areas[labels.len() * 2 + 1];
                let style: Style = if let ReplayInfoEditorFocus::SaveChanges = focus {
                    Color::Green.into()
                } else {
                    Color::Black.into()
                };
                let line = Line::raw("Save Changes").style(style).bold();
                frame.render_widget(line, area);

                match focus {
                    ReplayInfoEditorFocus::LabelData(n) => {
                        let area = areas[*n * 2];
                        let label = &labels[*n];
                        let cursor_offset = label.data.cursor();
                        frame.set_cursor_position(area.offset(layout::Offset {
                            x: label.label.name.len() as i32 + 2 + cursor_offset as i32,
                            y: 0,
                        }));
                    }

                    ReplayInfoEditorFocus::SaveChanges
                    | ReplayInfoEditorFocus::LabelRemove(_)
                    | ReplayInfoEditorFocus::LabelAdd => (),
                }
            }
        }
    }
}
