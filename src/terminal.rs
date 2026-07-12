use std::{
    cell::{Cell, RefCell},
    io::{Read as _, Write as _},
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
};

use crate::perf::{SlowOperation, UI_STALL_THRESHOLD};
use crate::theme::{self, ThemeKind};

use anyhow::{Context as _, Result};
use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, FontStyle, FontWeight, IntoElement, KeyDownEvent,
    KeyUpEvent, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad,
    Pixels, Render, ScrollDelta, ScrollWheelEvent, ShapedLine, SharedString, StrikethroughStyle,
    Subscription, TextRun, UTF16Selection, UnderlineStyle, Window, div, fill, outline, point,
    prelude::*, px, relative, rgb, size,
};
use libghostty_vt::{
    RenderState, Terminal, TerminalOptions, key, paste,
    render::{CellIterator, CursorVisualStyle, Dirty, RowIterator},
    screen::CellWide,
    selection::{
        FormatOptions as SelectionFormatOptions,
        gesture::{DragEvent, Geometry, Gesture, PressEvent, ReleaseEvent},
    },
    style::{RgbColor, Underline as GhosttyUnderline},
    terminal::{
        ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Mode, Point,
        PointCoordinate, PrimaryDeviceAttributes, ScrollViewport, SecondaryDeviceAttributes,
        SizeReportSize,
    },
};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

const INITIAL_COLUMNS: usize = 80;
const INITIAL_LINES: usize = 24;
const INITIAL_CELL_WIDTH: u16 = 8;
const INITIAL_CELL_HEIGHT: u16 = 18;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalSize {
    pub columns: usize,
    pub lines: usize,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl TerminalSize {
    pub fn new(columns: usize, lines: usize, cell_width: u16, cell_height: u16) -> Self {
        Self {
            columns: columns.max(2),
            lines: lines.max(1),
            cell_width: cell_width.max(1),
            cell_height: cell_height.max(1),
        }
    }

    fn pty_size(self) -> PtySize {
        PtySize {
            rows: self.lines.min(u16::MAX as usize) as u16,
            cols: self.columns.min(u16::MAX as usize) as u16,
            pixel_width: self
                .columns
                .saturating_mul(self.cell_width as usize)
                .min(u16::MAX as usize) as u16,
            pixel_height: self
                .lines
                .saturating_mul(self.cell_height as usize)
                .min(u16::MAX as usize) as u16,
        }
    }
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self::new(
            INITIAL_COLUMNS,
            INITIAL_LINES,
            INITIAL_CELL_WIDTH,
            INITIAL_CELL_HEIGHT,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalColor {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl TerminalColor {
    const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }
}

impl From<RgbColor> for TerminalColor {
    fn from(value: RgbColor) -> Self {
        Self::new(value.r, value.g, value.b)
    }
}

#[derive(Clone, Debug)]
pub struct TerminalCell {
    pub line: usize,
    pub column: usize,
    pub text: String,
    pub foreground: TerminalColor,
    pub background: TerminalColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub wide: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalCursor {
    pub line: usize,
    pub column: usize,
    pub shape: TerminalCursorShape,
    pub visible: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalCursorShape {
    Bar,
    Block,
    Underline,
    HollowBlock,
}

#[derive(Clone, Debug)]
pub struct TerminalSnapshot {
    pub size: TerminalSize,
    pub cells: Vec<TerminalCell>,
    pub cursor: TerminalCursor,
    pub title: String,
    pub exited: bool,
}

impl TerminalSnapshot {
    fn empty(size: TerminalSize) -> Self {
        Self {
            size,
            cells: Vec::new(),
            cursor: TerminalCursor {
                line: 0,
                column: 0,
                shape: TerminalCursorShape::Block,
                visible: true,
            },
            title: "Terminal".to_owned(),
            exited: false,
        }
    }
}

#[derive(Debug)]
enum TerminalEvent {
    Snapshot(TerminalSnapshot),
    ClipboardStore(String),
    Bell,
    Exited,
    Error(String),
}

pub struct TerminalCore {
    commands: mpsc::Sender<WorkerCommand>,
    events: async_channel::Receiver<TerminalEvent>,
    size: Mutex<TerminalSize>,
}

impl TerminalCore {
    pub fn start(working_directory: &Path) -> Result<Arc<Self>> {
        let size = TerminalSize::default();
        let (commands, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = async_channel::unbounded();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let cwd = working_directory.to_owned();
        let reader_commands = commands.clone();
        std::thread::Builder::new()
            .name("Rode Ghostty terminal".to_owned())
            .spawn(move || {
                match TerminalWorker::new(cwd, size, reader_commands, event_tx.clone()) {
                    Ok(mut worker) => {
                        let _ = ready_tx.send(Ok(()));
                        if let Err(error) = worker.run(command_rx) {
                            let _ = event_tx.send_blocking(TerminalEvent::Error(format!(
                                "Ghostty terminal worker: {error:#}"
                            )));
                        }
                    }
                    Err(error) => {
                        let detail = format!("{error:#}");
                        let _ = ready_tx.send(Err(detail.clone()));
                        let _ = event_tx.send_blocking(TerminalEvent::Error(detail));
                    }
                }
            })
            .context("spawning the Ghostty terminal worker")?;
        ready_rx
            .recv()
            .context("Ghostty terminal worker stopped during startup")?
            .map_err(anyhow::Error::msg)?;

        Ok(Arc::new(Self {
            commands,
            events: event_rx,
            size: Mutex::new(size),
        }))
    }

    fn events(&self) -> async_channel::Receiver<TerminalEvent> {
        self.events.clone()
    }

    pub fn input(&self, bytes: impl Into<Vec<u8>>) {
        let _ = self.commands.send(WorkerCommand::Text(bytes.into()));
    }

    pub fn paste(&self, text: &str) {
        let _ = self.commands.send(WorkerCommand::Paste(text.to_owned()));
    }

    pub fn resize(&self, size: TerminalSize) {
        let mut current = self.size.lock().expect("terminal size mutex poisoned");
        if *current == size {
            return;
        }
        *current = size;
        drop(current);
        let _ = self.commands.send(WorkerCommand::Resize(size));
    }

    pub fn scroll_lines(&self, lines: i32) {
        let _ = self.commands.send(WorkerCommand::Scroll(lines));
    }

    pub fn copy_selection(&self) {
        let _ = self.commands.send(WorkerCommand::CopySelection);
    }

    fn selection_pointer(&self, event: SelectionPointerEvent) {
        let _ = self.commands.send(WorkerCommand::SelectionPointer(event));
    }

    pub fn size(&self) -> TerminalSize {
        *self.size.lock().expect("terminal size mutex poisoned")
    }

    fn send_key(&self, event: TerminalKeyEvent) {
        let _ = self.commands.send(WorkerCommand::Key(event));
    }

    fn focus(&self, focused: bool) {
        let _ = self.commands.send(WorkerCommand::Focus(focused));
    }
}

#[derive(Debug)]
enum WorkerCommand {
    PtyOutput(Vec<u8>),
    PtyClosed,
    Text(Vec<u8>),
    Paste(String),
    Key(TerminalKeyEvent),
    Focus(bool),
    Resize(TerminalSize),
    Scroll(i32),
    CopySelection,
    SelectionPointer(SelectionPointerEvent),
    Shutdown,
}

#[derive(Clone, Copy, Debug)]
enum SelectionPointerPhase {
    Press,
    Drag,
    Release,
}

#[derive(Clone, Copy, Debug)]
struct SelectionPointerEvent {
    phase: SelectionPointerPhase,
    column: u16,
    row: u32,
    x: f64,
    y: f64,
    rectangle: bool,
    geometry: Geometry,
}

#[derive(Debug)]
struct TerminalKeyEvent {
    key: key::Key,
    action: key::Action,
    mods: key::Mods,
    consumed_mods: key::Mods,
    unshifted: char,
    text: Option<String>,
}

struct TerminalWorker {
    terminal: Terminal<'static, 'static>,
    renderer: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    key_encoder: key::Encoder<'static>,
    key_event: key::Event<'static>,
    selection_gesture: Gesture<'static>,
    selection_press: PressEvent<'static>,
    selection_drag: DragEvent<'static>,
    selection_release: ReleaseEvent<'static>,
    started_at: std::time::Instant,
    writer: Rc<RefCell<Box<dyn std::io::Write + Send>>>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    size: Rc<Cell<TerminalSize>>,
    events: async_channel::Sender<TerminalEvent>,
    exited: bool,
}

impl TerminalWorker {
    fn new(
        cwd: PathBuf,
        size: TerminalSize,
        commands: mpsc::Sender<WorkerCommand>,
        events: async_channel::Sender<TerminalEvent>,
    ) -> Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(size.pty_size())
            .with_context(|| format!("opening host PTY in {}", cwd.display()))?;
        let shell = user_shell();
        let mut command = CommandBuilder::new(&shell);
        command.cwd(&cwd);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env("TERM_PROGRAM", "Rode");
        command.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        let child = pair
            .slave
            .spawn_command(command)
            .with_context(|| format!("starting {}", shell.display()))?;
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .context("cloning PTY reader")?;
        std::thread::Builder::new()
            .name("Rode terminal PTY reader".to_owned())
            .spawn(move || {
                let mut buffer = [0u8; 16 * 1024];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => {
                            if commands
                                .send(WorkerCommand::PtyOutput(buffer[..count].to_vec()))
                                .is_err()
                            {
                                return;
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                let _ = commands.send(WorkerCommand::PtyClosed);
            })
            .context("spawning PTY reader")?;

        let writer = Rc::new(RefCell::new(
            pair.master.take_writer().context("taking PTY writer")?,
        ));
        let shared_size = Rc::new(Cell::new(size));
        let mut terminal = Terminal::new(TerminalOptions {
            cols: size.columns as u16,
            rows: size.lines as u16,
            max_scrollback: 10_000,
        })?;
        terminal.resize(
            size.columns as u16,
            size.lines as u16,
            size.cell_width.into(),
            size.cell_height.into(),
        )?;
        terminal
            .set_default_fg_color(Some(RgbColor {
                r: 216,
                g: 219,
                b: 226,
            }))?
            .set_default_bg_color(Some(RgbColor {
                r: 15,
                g: 17,
                b: 21,
            }))?
            .set_default_cursor_color(Some(RgbColor {
                r: 216,
                g: 219,
                b: 226,
            }))?;
        terminal
            .on_pty_write({
                let writer = writer.clone();
                move |_, data| {
                    let _ = writer.borrow_mut().write_all(data);
                    let _ = writer.borrow_mut().flush();
                }
            })?
            .on_bell({
                let events = events.clone();
                move |_| {
                    let _ = events.try_send(TerminalEvent::Bell);
                }
            })?
            .on_size({
                let size = shared_size.clone();
                move |_| {
                    let size = size.get();
                    Some(SizeReportSize {
                        rows: size.lines as u16,
                        columns: size.columns as u16,
                        cell_width: size.cell_width.into(),
                        cell_height: size.cell_height.into(),
                    })
                }
            })?
            .on_xtversion(|_| Some("Rode/libghostty-vt"))?
            .on_color_scheme(|_| None)?
            .on_device_attributes(|_| {
                Some(DeviceAttributes {
                    primary: PrimaryDeviceAttributes::new(
                        ConformanceLevel::VT220,
                        &[
                            DeviceAttributeFeature::COLUMNS_132,
                            DeviceAttributeFeature::SELECTIVE_ERASE,
                            DeviceAttributeFeature::ANSI_COLOR,
                        ],
                    ),
                    secondary: SecondaryDeviceAttributes {
                        device_type: DeviceType::VT220,
                        firmware_version: 1,
                        rom_cartridge: 0,
                    },
                    tertiary: Default::default(),
                })
            })?;

        Ok(Self {
            terminal,
            renderer: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            key_encoder: key::Encoder::new()?,
            key_event: key::Event::new()?,
            selection_gesture: Gesture::new()?,
            selection_press: PressEvent::new()?,
            selection_drag: DragEvent::new()?,
            selection_release: ReleaseEvent::new()?,
            started_at: std::time::Instant::now(),
            writer,
            master: pair.master,
            child,
            size: shared_size,
            events,
            exited: false,
        })
    }

    fn run(&mut self, commands: mpsc::Receiver<WorkerCommand>) -> Result<()> {
        self.publish_snapshot()?;
        while let Ok(command) = commands.recv() {
            match command {
                WorkerCommand::PtyOutput(output) => {
                    {
                        let _timing = SlowOperation::new(
                            "terminal.pty_parse",
                            UI_STALL_THRESHOLD,
                            format!("bytes={}", output.len()),
                        );
                        self.terminal.vt_write(&output);
                    }
                    self.publish_snapshot()?;
                }
                WorkerCommand::PtyClosed => {
                    self.exited = true;
                    let _ = self.child.try_wait();
                    self.publish_snapshot()?;
                    let _ = self.events.send_blocking(TerminalEvent::Exited);
                }
                WorkerCommand::Text(text) => self.write_pty(&text),
                WorkerCommand::Paste(text) => self.write_paste(text)?,
                WorkerCommand::Key(event) => self.write_key(event)?,
                WorkerCommand::Focus(focused) => self.write_focus(focused)?,
                WorkerCommand::Resize(size) => {
                    self.size.set(size);
                    self.terminal.resize(
                        size.columns as u16,
                        size.lines as u16,
                        size.cell_width.into(),
                        size.cell_height.into(),
                    )?;
                    self.master.resize(size.pty_size())?;
                    self.publish_snapshot()?;
                }
                WorkerCommand::Scroll(lines) => {
                    self.terminal
                        .scroll_viewport(ScrollViewport::Delta(lines as isize));
                    self.publish_snapshot()?;
                }
                WorkerCommand::CopySelection => self.copy_selection()?,
                WorkerCommand::SelectionPointer(event) => {
                    self.update_selection(event)?;
                    self.publish_snapshot()?;
                }
                WorkerCommand::Shutdown => {
                    let _ = self.child.kill();
                    break;
                }
            }
        }
        Ok(())
    }

    fn write_pty(&self, data: &[u8]) {
        let _ = self.writer.borrow_mut().write_all(data);
        let _ = self.writer.borrow_mut().flush();
    }

    fn write_key(&mut self, event: TerminalKeyEvent) -> Result<()> {
        self.key_event
            .set_action(event.action)
            .set_key(event.key)
            .set_mods(event.mods)
            .set_consumed_mods(event.consumed_mods)
            .set_unshifted_codepoint(event.unshifted)
            .set_utf8(event.text);
        let mut encoded = Vec::with_capacity(32);
        self.key_encoder
            .set_options_from_terminal(&self.terminal)
            .encode_to_vec(&self.key_event, &mut encoded)?;
        self.write_pty(&encoded);
        Ok(())
    }

    fn write_focus(&self, focused: bool) -> Result<()> {
        if !self.terminal.mode(Mode::FOCUS_EVENT)? {
            return Ok(());
        }
        let mut buffer = [0u8; 8];
        let event = if focused {
            libghostty_vt::focus::Event::Gained
        } else {
            libghostty_vt::focus::Event::Lost
        };
        let written = event.encode(&mut buffer)?;
        self.write_pty(&buffer[..written]);
        Ok(())
    }

    fn write_paste(&self, text: String) -> Result<()> {
        let mut input = text.into_bytes();
        let bracketed = self.terminal.mode(Mode::BRACKETED_PASTE)?;
        let mut output = vec![0u8; input.len().saturating_mul(2).saturating_add(32)];
        match paste::encode(&mut input, bracketed, &mut output) {
            Ok(written) => self.write_pty(&output[..written]),
            Err(libghostty_vt::Error::OutOfSpace { required }) => {
                output.resize(required, 0);
                let written = paste::encode(&mut input, bracketed, &mut output)?;
                self.write_pty(&output[..written]);
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    fn copy_selection(&self) -> Result<()> {
        let options = SelectionFormatOptions::new()
            .with_emit_format(libghostty_vt::fmt::Format::Plain)
            .with_unwrap(true)
            .with_trim(true);
        let Some(bytes) = self.terminal.format_selection_alloc(None, options)? else {
            return Ok(());
        };
        let text = String::from_utf8_lossy(&bytes).into_owned();
        if !text.is_empty() {
            let _ = self.events.try_send(TerminalEvent::ClipboardStore(text));
        }
        Ok(())
    }

    fn update_selection(&mut self, event: SelectionPointerEvent) -> Result<()> {
        let grid_ref = self.terminal.grid_ref(Point::Viewport(PointCoordinate {
            x: event.column,
            y: event.row,
        }))?;
        match event.phase {
            SelectionPointerPhase::Press => {
                self.selection_press
                    .set_position(event.x, event.y)?
                    .set_time(self.started_at.elapsed())?
                    .set_repeat_distance(5.)?
                    .set_repeat_interval(std::time::Duration::from_millis(500))?;
                let selection = self.selection_press.apply(
                    &mut self.selection_gesture,
                    &self.terminal,
                    grid_ref,
                )?;
                self.terminal.set_selection(selection.as_ref())?;
            }
            SelectionPointerPhase::Drag => {
                self.selection_drag
                    .set_position(event.x, event.y)?
                    .set_rectangle(event.rectangle)?;
                let selection = self.selection_drag.apply(
                    &mut self.selection_gesture,
                    &self.terminal,
                    grid_ref,
                    event.geometry,
                )?;
                self.terminal.set_selection(selection.as_ref())?;
            }
            SelectionPointerPhase::Release => {
                self.selection_release.apply(
                    &mut self.selection_gesture,
                    &self.terminal,
                    Some(grid_ref),
                )?;
            }
        }
        Ok(())
    }

    fn publish_snapshot(&mut self) -> Result<()> {
        let size = self.size.get();
        let _timing = SlowOperation::new(
            "terminal.snapshot",
            UI_STALL_THRESHOLD,
            format!(
                "rows={} columns={} grid_cells={}",
                size.lines,
                size.columns,
                size.lines.saturating_mul(size.columns)
            ),
        );
        let snapshot = ghostty_snapshot(
            &self.terminal,
            &mut self.renderer,
            &mut self.rows,
            &mut self.cells,
            size,
            self.exited,
        )?;
        let _ = self.events.try_send(TerminalEvent::Snapshot(snapshot));
        Ok(())
    }
}

fn ghostty_snapshot<'a>(
    terminal: &Terminal<'a, 'a>,
    renderer: &mut RenderState<'a>,
    rows: &mut RowIterator<'a>,
    cells: &mut CellIterator<'a>,
    size: TerminalSize,
    exited: bool,
) -> Result<TerminalSnapshot> {
    let render = renderer.update(terminal)?;
    let colors = render.colors()?;
    let cursor = render.cursor_viewport()?;
    let cursor_shape = match render.cursor_visual_style()? {
        CursorVisualStyle::Bar => TerminalCursorShape::Bar,
        CursorVisualStyle::Block => TerminalCursorShape::Block,
        CursorVisualStyle::Underline => TerminalCursorShape::Underline,
        CursorVisualStyle::BlockHollow => TerminalCursorShape::HollowBlock,
        _ => TerminalCursorShape::Block,
    };
    let mut snapshot = TerminalSnapshot::empty(TerminalSize::new(
        render.cols()?.into(),
        render.rows()?.into(),
        size.cell_width,
        size.cell_height,
    ));
    snapshot.title = terminal
        .title()
        .ok()
        .filter(|title| !title.is_empty())
        .unwrap_or("Terminal")
        .to_owned();
    snapshot.exited = exited;
    snapshot.cursor = TerminalCursor {
        line: cursor.map_or(0, |cursor| cursor.y as usize),
        column: cursor.map_or(0, |cursor| cursor.x as usize),
        shape: cursor_shape,
        visible: render.cursor_visible()?,
    };

    let mut row_index = 0usize;
    let mut row_iter = rows.update(&render)?;
    while let Some(row) = row_iter.next() {
        let mut column = 0usize;
        let mut cell_iter = cells.update(row)?;
        while let Some(cell) = cell_iter.next() {
            let raw = cell.raw_cell()?;
            let wide = raw.wide()?;
            let mut text = String::new();
            if cell.graphemes_len()? > 0 {
                cell.graphemes_utf8(&mut text)?;
            }
            let style = cell.style()?;
            let mut foreground = cell.fg_color()?.unwrap_or(colors.foreground);
            let mut background = cell.bg_color()?.unwrap_or(colors.background);
            if style.inverse || cell.is_selected()? {
                std::mem::swap(&mut foreground, &mut background);
            }
            if style.invisible {
                foreground = background;
            }
            if style.faint {
                foreground.r = ((foreground.r as u16 * 7) / 10) as u8;
                foreground.g = ((foreground.g as u16 * 7) / 10) as u8;
                foreground.b = ((foreground.b as u16 * 7) / 10) as u8;
            }
            snapshot.cells.push(TerminalCell {
                line: row_index,
                column,
                text,
                foreground: foreground.into(),
                background: background.into(),
                bold: style.bold,
                italic: style.italic,
                underline: style.underline != GhosttyUnderline::None,
                strikeout: style.strikethrough,
                wide: wide == CellWide::Wide,
            });
            column += 1;
        }
        row.set_dirty(false)?;
        row_index += 1;
    }
    render.set_dirty(Dirty::Clean)?;
    Ok(snapshot)
}

fn user_shell() -> PathBuf {
    std::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

pub struct TerminalView {
    core: Arc<TerminalCore>,
    pub focus_handle: FocusHandle,
    snapshot: TerminalSnapshot,
    title: String,
    exited: bool,
    marked_text: Option<String>,
    cursor_bounds: Option<Bounds<Pixels>>,
    grid_bounds: Option<Bounds<Pixels>>,
    cell_size: Option<gpui::Size<Pixels>>,
    theme: ThemeKind,
    _subscriptions: Vec<Subscription>,
}

impl TerminalView {
    pub fn new(
        core: Arc<TerminalCore>,
        theme: ThemeKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let focus_in = cx.on_focus(&focus_handle, window, |this, _, cx| {
            this.core.focus(true);
            cx.notify();
        });
        let focus_out = cx.on_blur(&focus_handle, window, |this, _, cx| {
            this.core.focus(false);
            this.marked_text = None;
            cx.notify();
        });
        let mut view = Self {
            snapshot: TerminalSnapshot::empty(core.size()),
            core,
            focus_handle,
            title: "Terminal".to_owned(),
            exited: false,
            marked_text: None,
            cursor_bounds: None,
            grid_bounds: None,
            cell_size: None,
            theme,
            _subscriptions: vec![focus_in, focus_out],
        };
        view.start_event_pump(cx);
        view
    }

    pub fn set_theme(&mut self, theme: ThemeKind, cx: &mut Context<Self>) {
        if self.theme != theme {
            self.theme = theme;
            cx.notify();
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn exited(&self) -> bool {
        self.exited
    }

    fn start_event_pump(&mut self, cx: &mut Context<Self>) {
        let events = self.core.events();
        cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this
                    .update(cx, |this, cx| this.handle_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn handle_event(&mut self, event: TerminalEvent, cx: &mut Context<Self>) {
        let context = match &event {
            TerminalEvent::Snapshot(snapshot) => format!(
                "kind=snapshot rows={} cells={}",
                snapshot.size.lines,
                snapshot.cells.len()
            ),
            TerminalEvent::ClipboardStore(text) => {
                format!("kind=clipboard bytes={}", text.len())
            }
            TerminalEvent::Bell => "kind=bell".to_owned(),
            TerminalEvent::Exited => "kind=exited".to_owned(),
            TerminalEvent::Error(_) => "kind=error".to_owned(),
        };
        let _timing = SlowOperation::new("ui.terminal_event_update", UI_STALL_THRESHOLD, context);
        match event {
            TerminalEvent::Snapshot(snapshot) => {
                self.title = snapshot.title.clone();
                self.exited = snapshot.exited;
                self.snapshot = snapshot;
            }
            TerminalEvent::ClipboardStore(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            TerminalEvent::Bell => {}
            TerminalEvent::Exited => self.exited = true,
            TerminalEvent::Error(error) => {
                self.title = format!("Terminal error · {error}");
                self.exited = true;
            }
        }
        cx.notify();
    }

    fn resize(
        &mut self,
        size: TerminalSize,
        cursor_bounds: Bounds<Pixels>,
        grid_bounds: Bounds<Pixels>,
        cell_size: gpui::Size<Pixels>,
    ) {
        self.cursor_bounds = Some(cursor_bounds);
        self.grid_bounds = Some(grid_bounds);
        self.cell_size = Some(cell_size);
        if self.core.size() != size {
            self.core.resize(size);
        }
    }

    fn key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;

        if modifiers.control && modifiers.shift && key.eq_ignore_ascii_case("v") {
            if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                self.core.paste(&text);
            }
            cx.stop_propagation();
            return;
        }
        if modifiers.control && modifiers.shift && key.eq_ignore_ascii_case("c") {
            self.core.copy_selection();
            cx.stop_propagation();
            return;
        }

        let Some((ghostty_key, unshifted, special)) = ghostty_key(key) else {
            return;
        };
        if !special && !modifiers.control && !modifiers.alt && !modifiers.platform {
            // Printable text is committed by the Wayland text-input/IME path.
            return;
        }
        let mods = ghostty_mods(modifiers);
        let consumed_mods = if event.keystroke.key_char.is_some() && modifiers.shift {
            key::Mods::SHIFT
        } else {
            key::Mods::empty()
        };
        self.core.send_key(TerminalKeyEvent {
            key: ghostty_key,
            action: if event.is_held {
                key::Action::Repeat
            } else {
                key::Action::Press
            },
            mods,
            consumed_mods,
            unshifted,
            text: (!special)
                .then(|| event.keystroke.key_char.clone())
                .flatten(),
        });
        cx.stop_propagation();
    }

    fn key_up(&mut self, event: &KeyUpEvent, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        let Some((ghostty_key, unshifted, special)) = ghostty_key(key) else {
            return;
        };
        if !special && !modifiers.control && !modifiers.alt && !modifiers.platform {
            return;
        }
        self.core.send_key(TerminalKeyEvent {
            key: ghostty_key,
            action: key::Action::Release,
            mods: ghostty_mods(modifiers),
            consumed_mods: key::Mods::empty(),
            unshifted,
            text: None,
        });
        cx.stop_propagation();
    }

    fn pointer_event(
        &self,
        position: gpui::Point<Pixels>,
        modifiers: gpui::Modifiers,
        phase: SelectionPointerPhase,
    ) -> Option<SelectionPointerEvent> {
        let bounds = self.grid_bounds?;
        let cell_size = self.cell_size?;
        let width = f32::from(cell_size.width).max(1.);
        let height = f32::from(cell_size.height).max(1.);
        let x = (f32::from(position.x - bounds.left())).clamp(0., f32::from(bounds.size.width));
        let y = (f32::from(position.y - bounds.top())).clamp(0., f32::from(bounds.size.height));
        let terminal_size = self.core.size();
        Some(SelectionPointerEvent {
            phase,
            column: ((x / width).floor() as usize).min(terminal_size.columns.saturating_sub(1))
                as u16,
            row: ((y / height).floor() as usize).min(terminal_size.lines.saturating_sub(1)) as u32,
            x: x as f64,
            y: y as f64,
            rectangle: modifiers.alt,
            geometry: Geometry {
                columns: terminal_size.columns.min(u32::MAX as usize) as u32,
                cell_width: width.ceil() as u32,
                padding_left: 0,
                screen_height: f32::from(bounds.size.height).ceil().max(1.) as u32,
            },
        })
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        if let Some(event) = self.pointer_event(
            event.position,
            event.modifiers,
            SelectionPointerPhase::Press,
        ) {
            self.core.selection_pointer(event);
        }
        cx.stop_propagation();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        if let Some(event) =
            self.pointer_event(event.position, event.modifiers, SelectionPointerPhase::Drag)
        {
            self.core.selection_pointer(event);
        }
        cx.stop_propagation();
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        if let Some(event) = self.pointer_event(
            event.position,
            event.modifiers,
            SelectionPointerPhase::Release,
        ) {
            self.core.selection_pointer(event);
        }
        cx.stop_propagation();
    }

    fn scroll(&mut self, delta: ScrollDelta, cx: &mut Context<Self>) {
        let lines = match delta {
            ScrollDelta::Lines(point) => point.y.round() as i32,
            ScrollDelta::Pixels(point) => (f32::from(point.y) / 18.).round() as i32,
        };
        if lines != 0 {
            self.core.scroll_lines(lines);
            cx.notify();
        }
    }
}

fn ghostty_mods(modifiers: gpui::Modifiers) -> key::Mods {
    let mut result = key::Mods::empty();
    if modifiers.shift {
        result |= key::Mods::SHIFT;
    }
    if modifiers.alt {
        result |= key::Mods::ALT;
    }
    if modifiers.control {
        result |= key::Mods::CTRL;
    }
    if modifiers.platform {
        result |= key::Mods::SUPER;
    }
    result
}

fn ghostty_key(name: &str) -> Option<(key::Key, char, bool)> {
    let (key, unshifted, special) = match name {
        "a" => (key::Key::A, 'a', false),
        "b" => (key::Key::B, 'b', false),
        "c" => (key::Key::C, 'c', false),
        "d" => (key::Key::D, 'd', false),
        "e" => (key::Key::E, 'e', false),
        "f" => (key::Key::F, 'f', false),
        "g" => (key::Key::G, 'g', false),
        "h" => (key::Key::H, 'h', false),
        "i" => (key::Key::I, 'i', false),
        "j" => (key::Key::J, 'j', false),
        "k" => (key::Key::K, 'k', false),
        "l" => (key::Key::L, 'l', false),
        "m" => (key::Key::M, 'm', false),
        "n" => (key::Key::N, 'n', false),
        "o" => (key::Key::O, 'o', false),
        "p" => (key::Key::P, 'p', false),
        "q" => (key::Key::Q, 'q', false),
        "r" => (key::Key::R, 'r', false),
        "s" => (key::Key::S, 's', false),
        "t" => (key::Key::T, 't', false),
        "u" => (key::Key::U, 'u', false),
        "v" => (key::Key::V, 'v', false),
        "w" => (key::Key::W, 'w', false),
        "x" => (key::Key::X, 'x', false),
        "y" => (key::Key::Y, 'y', false),
        "z" => (key::Key::Z, 'z', false),
        "0" => (key::Key::Digit0, '0', false),
        "1" => (key::Key::Digit1, '1', false),
        "2" => (key::Key::Digit2, '2', false),
        "3" => (key::Key::Digit3, '3', false),
        "4" => (key::Key::Digit4, '4', false),
        "5" => (key::Key::Digit5, '5', false),
        "6" => (key::Key::Digit6, '6', false),
        "7" => (key::Key::Digit7, '7', false),
        "8" => (key::Key::Digit8, '8', false),
        "9" => (key::Key::Digit9, '9', false),
        "space" => (key::Key::Space, ' ', false),
        "enter" => (key::Key::Enter, '\0', true),
        "backspace" => (key::Key::Backspace, '\0', true),
        "tab" => (key::Key::Tab, '\0', true),
        "escape" => (key::Key::Escape, '\0', true),
        "up" => (key::Key::ArrowUp, '\0', true),
        "down" => (key::Key::ArrowDown, '\0', true),
        "left" => (key::Key::ArrowLeft, '\0', true),
        "right" => (key::Key::ArrowRight, '\0', true),
        "home" => (key::Key::Home, '\0', true),
        "end" => (key::Key::End, '\0', true),
        "insert" => (key::Key::Insert, '\0', true),
        "delete" => (key::Key::Delete, '\0', true),
        "pageup" => (key::Key::PageUp, '\0', true),
        "pagedown" => (key::Key::PageDown, '\0', true),
        "f1" => (key::Key::F1, '\0', true),
        "f2" => (key::Key::F2, '\0', true),
        "f3" => (key::Key::F3, '\0', true),
        "f4" => (key::Key::F4, '\0', true),
        "f5" => (key::Key::F5, '\0', true),
        "f6" => (key::Key::F6, '\0', true),
        "f7" => (key::Key::F7, '\0', true),
        "f8" => (key::Key::F8, '\0', true),
        "f9" => (key::Key::F9, '\0', true),
        "f10" => (key::Key::F10, '\0', true),
        "f11" => (key::Key::F11, '\0', true),
        "f12" => (key::Key::F12, '\0', true),
        _ => return None,
    };
    Some((key, unshifted, special))
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_text
            .as_ref()
            .map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked_text = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = None;
        self.core.input(new_text.as_bytes().to_vec());
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = Some(new_text.to_owned());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.cursor_bounds
    }

    fn character_index_for_point(
        &mut self,
        _: gpui::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(0)
    }
}

impl Render for TerminalView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::tokens(self.theme).colors;
        div()
            .id("native-terminal")
            .key_context("Terminal")
            .track_focus(&self.focus_handle)
            .w_full()
            .h(px(250.))
            .overflow_hidden()
            .bg(rgb(colors.root))
            .font_family("monospace")
            .text_size(px(13.))
            .line_height(px(18.))
            .on_click(cx.listener(|this, _, window, cx| {
                window.focus(&this.focus_handle, cx);
            }))
            .on_key_down(cx.listener(|this, event, _, cx| this.key_down(event, cx)))
            .on_key_up(cx.listener(|this, event, _, cx| this.key_up(event, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event, window, cx| this.mouse_down(event, window, cx)),
            )
            .on_mouse_move(cx.listener(|this, event, _, cx| this.mouse_move(event, cx)))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event, _, cx| this.mouse_up(event, cx)),
            )
            .on_scroll_wheel(
                cx.listener(|this, event: &ScrollWheelEvent, _, cx| this.scroll(event.delta, cx)),
            )
            .child(TerminalGrid {
                view: cx.entity(),
                snapshot: self.snapshot.clone(),
                marked_text: self.marked_text.clone(),
                theme: self.theme,
            })
    }
}

struct TerminalGrid {
    view: Entity<TerminalView>,
    snapshot: TerminalSnapshot,
    marked_text: Option<String>,
    theme: ThemeKind,
}

struct TerminalGridPrepaint {
    lines: Vec<ShapedLine>,
    backgrounds: Vec<PaintQuad>,
    cursor: Option<PaintQuad>,
    marked_text: Option<(ShapedLine, gpui::Point<Pixels>)>,
    origin: gpui::Point<Pixels>,
    line_height: Pixels,
}

impl IntoElement for TerminalGrid {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalGrid {
    type RequestLayoutState = ();
    type PrepaintState = TerminalGridPrepaint;

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = gpui::Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let colors = theme::tokens(self.theme).colors;
        let style = window.text_style();
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line_height = window.line_height();
        let probe: SharedString = "M".into();
        let probe_run = TextRun {
            len: 1,
            font: style.font(),
            color: style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let cell_width = window
            .text_system()
            .shape_line(probe, font_size, &[probe_run], None)
            .width()
            .max(px(1.));
        let columns = (f32::from(bounds.size.width) / f32::from(cell_width)).floor() as usize;
        let rows = (f32::from(bounds.size.height) / f32::from(line_height)).floor() as usize;
        let terminal_size = TerminalSize::new(
            columns,
            rows,
            f32::from(cell_width).ceil().min(u16::MAX as f32) as u16,
            f32::from(line_height).ceil().min(u16::MAX as f32) as u16,
        );
        let cursor_origin = point(
            bounds.left() + cell_width * self.snapshot.cursor.column as f32,
            bounds.top() + line_height * self.snapshot.cursor.line as f32,
        );
        let cursor_bounds = Bounds::new(cursor_origin, size(cell_width, line_height));
        self.view.update(cx, |view, _| {
            view.resize(
                terminal_size,
                cursor_bounds,
                bounds,
                size(cell_width, line_height),
            );
        });

        let grid_size = self.snapshot.size;
        let mut grid = vec![None; grid_size.lines.saturating_mul(grid_size.columns)];
        for cell in &self.snapshot.cells {
            if cell.line < grid_size.lines && cell.column < grid_size.columns {
                grid[cell.line * grid_size.columns + cell.column] = Some(cell);
            }
        }

        let mut lines = Vec::with_capacity(grid_size.lines);
        let mut backgrounds = Vec::new();
        for line in 0..grid_size.lines {
            let mut text = String::new();
            let mut runs = Vec::new();
            let mut column = 0;
            while column < grid_size.columns {
                let cell = grid[line * grid_size.columns + column];
                let (cell_text, foreground, background, wide, bold, italic, underline, strikeout) =
                    cell.map_or_else(
                        || {
                            (
                                " ",
                                TerminalColor::new(216, 219, 226),
                                TerminalColor::new(15, 17, 21),
                                false,
                                false,
                                false,
                                false,
                                false,
                            )
                        },
                        |cell| {
                            (
                                cell.text.as_str(),
                                cell.foreground,
                                cell.background,
                                cell.wide,
                                cell.bold,
                                cell.italic,
                                cell.underline,
                                cell.strikeout,
                            )
                        },
                    );
                text.push_str(cell_text);
                let foreground = color_to_gpui(foreground);
                let mut font = style.font();
                if bold {
                    font.weight = FontWeight::BOLD;
                }
                if italic {
                    font.style = FontStyle::Italic;
                }
                runs.push(TextRun {
                    len: cell_text.len(),
                    font,
                    color: foreground,
                    background_color: None,
                    underline: underline.then_some(UnderlineStyle {
                        color: Some(foreground),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    strikethrough: strikeout.then_some(StrikethroughStyle {
                        color: Some(foreground),
                        thickness: px(1.),
                    }),
                });
                if background != TerminalColor::new(15, 17, 21) {
                    backgrounds.push(fill(
                        Bounds::new(
                            point(
                                bounds.left() + cell_width * column as f32,
                                bounds.top() + line_height * line as f32,
                            ),
                            size(cell_width * if wide { 2. } else { 1. }, line_height),
                        ),
                        color_to_gpui(background),
                    ));
                }
                column += if wide { 2 } else { 1 };
            }
            let text: SharedString = text.into();
            lines.push(
                window
                    .text_system()
                    .shape_line(text, font_size, &runs, None),
            );
        }

        let cursor = if !self.snapshot.cursor.visible {
            None
        } else {
            match self.snapshot.cursor.shape {
                TerminalCursorShape::Underline => Some(fill(
                    Bounds::new(
                        point(cursor_origin.x, cursor_origin.y + line_height - px(2.)),
                        size(cell_width, px(2.)),
                    ),
                    rgb(colors.text),
                )),
                TerminalCursorShape::Bar => Some(fill(
                    Bounds::new(cursor_origin, size(px(2.), line_height)),
                    rgb(colors.text),
                )),
                TerminalCursorShape::HollowBlock => Some(outline(
                    cursor_bounds,
                    rgb(colors.text),
                    gpui::BorderStyle::default(),
                )),
                TerminalCursorShape::Block => {
                    Some(fill(cursor_bounds, rgb(colors.text).alpha(0.35)))
                }
            }
        };
        let marked_text = self
            .marked_text
            .as_ref()
            .filter(|text| !text.is_empty())
            .map(|text| {
                let text: SharedString = text.clone().into();
                let run = TextRun {
                    len: text.len(),
                    font: style.font(),
                    color: rgb(colors.text).into(),
                    background_color: Some(rgb(colors.overlay).into()),
                    underline: Some(UnderlineStyle {
                        color: Some(rgb(colors.focus_ring).into()),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    strikethrough: None,
                };
                (
                    window
                        .text_system()
                        .shape_line(text, font_size, &[run], None),
                    cursor_origin,
                )
            });

        TerminalGridPrepaint {
            lines,
            backgrounds,
            cursor,
            marked_text,
            origin: bounds.origin,
            line_height,
        }
    }

    fn paint(
        &mut self,
        _: Option<&gpui::GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.view.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for background in prepaint.backgrounds.drain(..) {
                window.paint_quad(background);
            }
            for (index, line) in prepaint.lines.iter().enumerate() {
                line.paint(
                    point(
                        prepaint.origin.x,
                        prepaint.origin.y + prepaint.line_height * index as f32,
                    ),
                    prepaint.line_height,
                    gpui::TextAlign::Left,
                    None,
                    window,
                    cx,
                )
                .expect("painting terminal line");
            }
            if let Some(cursor) = prepaint.cursor.take() {
                window.paint_quad(cursor);
            }
            if let Some((marked_text, origin)) = prepaint.marked_text.take() {
                marked_text
                    .paint(
                        origin,
                        prepaint.line_height,
                        gpui::TextAlign::Left,
                        None,
                        window,
                        cx,
                    )
                    .expect("painting terminal IME text");
            }
        });
    }
}

fn color_to_gpui(color: TerminalColor) -> gpui::Hsla {
    rgb(((color.red as u32) << 16) | ((color.green as u32) << 8) | color.blue as u32).into()
}

impl Drop for TerminalCore {
    fn drop(&mut self) {
        let _ = self.commands.send(WorkerCommand::Shutdown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_size_has_safe_minimums() {
        assert_eq!(TerminalSize::new(0, 0, 0, 0), TerminalSize::new(2, 1, 1, 1));
    }

    #[test]
    fn user_shell_preserves_the_host_choice() {
        if let Some(shell) = std::env::var_os("SHELL").filter(|shell| !shell.is_empty()) {
            assert_eq!(user_shell(), PathBuf::from(shell));
        }
    }

    #[test]
    fn ghostty_selection_formats_plain_text() {
        let mut terminal = Terminal::new(TerminalOptions {
            cols: 20,
            rows: 2,
            max_scrollback: 100,
        })
        .expect("creating Ghostty terminal");
        terminal.vt_write(b"hello world");

        let start = terminal
            .grid_ref(Point::Viewport(PointCoordinate { x: 0, y: 0 }))
            .expect("selection start");
        let end = terminal
            .grid_ref(Point::Viewport(PointCoordinate { x: 4, y: 0 }))
            .expect("selection end");
        let selection = libghostty_vt::selection::Selection::new(start, end, false);
        terminal
            .set_selection(Some(&selection))
            .expect("installing selection");
        let bytes = terminal
            .format_selection_alloc(
                None,
                SelectionFormatOptions::new()
                    .with_emit_format(libghostty_vt::fmt::Format::Plain)
                    .with_unwrap(true)
                    .with_trim(true),
            )
            .expect("formatting selection")
            .expect("active selection");

        assert_eq!(&*bytes, b"hello");
    }

    #[test]
    #[ignore = "spawns the user's interactive shell"]
    fn pty_accepts_input_and_updates_the_grid() {
        let terminal = TerminalCore::start(Path::new("/tmp")).expect("starting PTY");
        terminal.input(b"printf rode-terminal-ready\\n".to_vec());
        let events = terminal.events();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut content = String::new();
        while std::time::Instant::now() < deadline {
            if let Ok(TerminalEvent::Snapshot(snapshot)) = events.try_recv() {
                content = snapshot
                    .cells
                    .into_iter()
                    .map(|cell| cell.text)
                    .collect::<String>();
                if content.contains("rode-terminal-ready") {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            content.contains("rode-terminal-ready"),
            "content: {content:?}"
        );
    }
}
