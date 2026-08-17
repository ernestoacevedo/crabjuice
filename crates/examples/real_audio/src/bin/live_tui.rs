use anyhow::{Context, Result};
use cpal::Host;
use crabjuice_audio::{AudioProcessor, ProcessContext};
use crabjuice_dsp::{
    DelayProcessor, DistortionProcessor, GainProcessor, OnePoleLowPass, PitchDetector,
    PitchEstimate,
};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use real_audio::{
    default_input_index, default_output_index, input_devices, output_devices, select_input_device,
    select_output_device, start_live_audio, start_tuner_audio, AudioStats, DeviceInfo,
    LiveAudioSession, SharedProcessor, TunerSession,
};
use std::collections::VecDeque;
use std::io::{self, Stdout};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn main() -> Result<()> {
    let mut terminal = TerminalGuard::enter()?;
    let result = run(terminal.terminal_mut());
    terminal.leave()?;
    result
}

fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    let host = cpal::default_host();
    let mut app = App::new(host)?;

    loop {
        app.update_tuner();
        terminal.draw(|frame| draw(frame, &app))?;

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => {
                    if app.handle_key(key)? {
                        break;
                    }
                }
                Event::Mouse(mouse) => app.handle_mouse(mouse, terminal.size()?)?,
                _ => {}
            }
        }
    }

    app.stop_all();
    Ok(())
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    active: bool,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("failed to enable raw mode")?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
            .context("failed to enter alternate screen")?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).context("failed to create terminal")?;
        Ok(Self {
            terminal,
            active: true,
        })
    }

    fn terminal_mut(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }

    fn leave(&mut self) -> Result<()> {
        if self.active {
            disable_raw_mode().context("failed to disable raw mode")?;
            execute!(
                self.terminal.backend_mut(),
                LeaveAlternateScreen,
                DisableMouseCapture
            )
            .context("failed to leave alternate screen")?;
            self.terminal
                .show_cursor()
                .context("failed to show cursor")?;
            self.active = false;
        }
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.leave();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Panel {
    Input,
    Output,
    Chain,
    Params,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppView {
    Live,
    Tuner,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TunerMode {
    Chromatic,
    Guitar,
}

impl TunerMode {
    fn toggled(self) -> Self {
        match self {
            Self::Chromatic => Self::Guitar,
            Self::Guitar => Self::Chromatic,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Chromatic => "Chromatic",
            Self::Guitar => "Guitar",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TunerReading {
    estimate: PitchEstimate,
    updated_at: Instant,
}

impl Panel {
    fn next(self) -> Self {
        match self {
            Self::Input => Self::Output,
            Self::Output => Self::Chain,
            Self::Chain => Self::Params,
            Self::Params => Self::Input,
        }
    }

    fn previous(self) -> Self {
        match self {
            Self::Input => Self::Params,
            Self::Output => Self::Input,
            Self::Chain => Self::Output,
            Self::Params => Self::Chain,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct AppLayout {
    status: Rect,
    input: Rect,
    output: Rect,
    chain: Rect,
    params: Rect,
    meters: Rect,
    help: Rect,
}

impl AppLayout {
    fn new(area: Rect) -> Self {
        let root = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(5),
                Constraint::Min(10),
                Constraint::Length(5),
                Constraint::Length(3),
            ])
            .split(area);
        let devices = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(root[1]);
        let workspace = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(root[2]);

        Self {
            status: root[0],
            input: devices[0],
            output: devices[1],
            chain: workspace[0],
            params: workspace[1],
            meters: root[3],
            help: root[4],
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct TunerLayout {
    status: Rect,
    input: Rect,
    mode: Rect,
    tuner: Rect,
    meter: Rect,
    help: Rect,
}

impl TunerLayout {
    fn new(area: Rect) -> Self {
        let root = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(5),
                Constraint::Length(3),
                Constraint::Min(8),
                Constraint::Length(3),
                Constraint::Length(3),
            ])
            .split(area);
        Self {
            status: root[0],
            input: root[1],
            mode: root[2],
            tuner: root[3],
            meter: root[4],
            help: root[5],
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ParamDrag {
    slot: usize,
    param: usize,
    slider: Rect,
    start_col: u16,
    start_ratio: f32,
}

impl ParamDrag {
    fn updated(mut self, column: u16, fine: bool) -> (f32, Self) {
        let ratio = if fine {
            let delta = i32::from(column) - i32::from(self.start_col);
            self.start_ratio + delta as f32 * 0.005
        } else {
            slider_ratio_at(self.slider, column)
        }
        .clamp(0.0, 1.0);

        if !fine {
            self.start_col = column;
            self.start_ratio = ratio;
        }
        (ratio, self)
    }
}

struct App {
    host: Host,
    input_devices: Vec<DeviceInfo>,
    output_devices: Vec<DeviceInfo>,
    selected_input: usize,
    selected_output: usize,
    selected_slot: usize,
    selected_param: usize,
    active_panel: Panel,
    slots: Vec<ProcessorSlot>,
    processor: SharedProcessor,
    session: Option<LiveAudioSession>,
    status: String,
    effect_picker: Option<usize>,
    param_drag: Option<ParamDrag>,
    view: AppView,
    tuner_mode: TunerMode,
    tuner_session: Option<TunerSession>,
    pitch_detector: Option<PitchDetector>,
    pitch_history: VecDeque<PitchEstimate>,
    tuner_reading: Option<TunerReading>,
    last_tuner_analysis: Instant,
}

impl App {
    fn new(host: Host) -> Result<Self> {
        let input_devices = input_devices(&host)?;
        let output_devices = output_devices(&host)?;
        let selected_input = default_input_index(&host, &input_devices).unwrap_or(0);
        let selected_output = default_output_index(&host, &output_devices).unwrap_or(0);
        let slots = vec![ProcessorSlot::gain()];
        let processor = build_shared_processor(&slots);

        Ok(Self {
            host,
            input_devices,
            output_devices,
            selected_input,
            selected_output,
            selected_slot: 0,
            selected_param: 0,
            active_panel: Panel::Input,
            slots,
            processor,
            session: None,
            status: "Stopped".to_string(),
            effect_picker: None,
            param_drag: None,
            view: AppView::Live,
            tuner_mode: TunerMode::Chromatic,
            tuner_session: None,
            pitch_detector: None,
            pitch_history: VecDeque::with_capacity(5),
            tuner_reading: None,
            last_tuner_analysis: Instant::now(),
        })
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        if key.code == KeyCode::Char('T') {
            self.toggle_view();
            return Ok(false);
        }

        if self.view == AppView::Tuner {
            return Ok(self.handle_tuner_key(key));
        }

        if self.effect_picker.is_some() {
            self.handle_effect_picker_key(key);
            return Ok(false);
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
            KeyCode::Tab => self.active_panel = self.active_panel.next(),
            KeyCode::BackTab => self.active_panel = self.active_panel.previous(),
            KeyCode::Char(' ') => self.toggle_stream()?,
            KeyCode::Char('r') => self.restart_stream()?,
            KeyCode::Char('a') => self.effect_picker = Some(0),
            KeyCode::Char('d') => self.toggle_slot(),
            KeyCode::Char('x') => self.delete_slot(),
            KeyCode::Char('t') => self.toggle_slot_kind(),
            KeyCode::Char('[') => self.move_param(-1),
            KeyCode::Char(']') => self.move_param(1),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Left => self.adjust_active_param(-key_adjustment(key.modifiers)),
            KeyCode::Right => self.adjust_active_param(key_adjustment(key.modifiers)),
            KeyCode::Enter => self.activate_selection()?,
            _ => {}
        }

        Ok(false)
    }

    fn handle_tuner_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Char('m') => self.tuner_mode = self.tuner_mode.toggled(),
            KeyCode::Char(' ') => self.toggle_tuner(),
            KeyCode::Char('r') => self.restart_tuner(),
            KeyCode::Up => self.move_tuner_input(-1),
            KeyCode::Down => self.move_tuner_input(1),
            _ => {}
        }
        false
    }

    fn handle_mouse(&mut self, mouse: MouseEvent, terminal_area: Rect) -> Result<()> {
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some(view) = view_at(terminal_area, mouse.column, mouse.row) {
                if view != self.view {
                    self.set_view(view);
                }
                return Ok(());
            }
        }
        if self.view == AppView::Tuner {
            self.handle_tuner_mouse(mouse, terminal_area);
            return Ok(());
        }

        let layout = AppLayout::new(terminal_area);

        if self.effect_picker.is_some() {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                let picker = effect_picker_area(terminal_area);
                if let Some(kind) = effect_kind_at(picker, mouse.column, mouse.row) {
                    self.add_slot(kind);
                    self.effect_picker = None;
                } else if !rect_contains(picker, mouse.column, mouse.row) {
                    self.effect_picker = None;
                }
            }
            return Ok(());
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.param_drag = None;
                if !self.begin_param_drag(mouse, layout) {
                    self.click_at(mouse.column, mouse.row, layout)?;
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                self.drag_param_at(mouse);
            }
            MouseEventKind::Up(MouseButton::Left) => self.param_drag = None,
            MouseEventKind::ScrollUp => self.scroll_at(
                mouse.column,
                mouse.row,
                -1,
                layout,
                mouse.modifiers.contains(KeyModifiers::SHIFT),
            ),
            MouseEventKind::ScrollDown => self.scroll_at(
                mouse.column,
                mouse.row,
                1,
                layout,
                mouse.modifiers.contains(KeyModifiers::SHIFT),
            ),
            _ => {}
        }

        Ok(())
    }

    fn handle_tuner_mouse(&mut self, mouse: MouseEvent, terminal_area: Rect) {
        let layout = TunerLayout::new(terminal_area);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(index) = visible_list_index_at(
                    layout.input,
                    mouse.column,
                    mouse.row,
                    self.input_devices.len(),
                    self.selected_input,
                ) {
                    self.selected_input = index;
                    self.restart_tuner();
                } else if rect_contains(layout.mode, mouse.column, mouse.row) {
                    self.tuner_mode = self.tuner_mode.toggled();
                }
            }
            MouseEventKind::ScrollUp if rect_contains(layout.input, mouse.column, mouse.row) => {
                self.move_tuner_input(-1);
            }
            MouseEventKind::ScrollDown if rect_contains(layout.input, mouse.column, mouse.row) => {
                self.move_tuner_input(1);
            }
            _ => {}
        }
    }

    fn click_at(&mut self, column: u16, row: u16, layout: AppLayout) -> Result<()> {
        if let Some(index) = visible_list_index_at(
            layout.input,
            column,
            row,
            self.input_devices.len(),
            self.selected_input,
        ) {
            self.active_panel = Panel::Input;
            self.selected_input = index;
        } else if let Some(index) = visible_list_index_at(
            layout.output,
            column,
            row,
            self.output_devices.len(),
            self.selected_output,
        ) {
            self.active_panel = Panel::Output;
            self.selected_output = index;
        } else if let Some(action) = chain_action_at(
            layout.chain,
            column,
            row,
            self.slots.len(),
            self.selected_slot,
        ) {
            self.handle_chain_action(action);
        } else if rect_contains(layout.params, column, row) {
            self.active_panel = Panel::Params;
            if let Some(index) = param_index_at(layout.params, row, self.selected_slot()) {
                self.selected_param = index;
            }
        }

        Ok(())
    }

    fn begin_param_drag(&mut self, mouse: MouseEvent, layout: AppLayout) -> bool {
        let Some(index) = param_index_at(layout.params, mouse.row, self.selected_slot()) else {
            return false;
        };
        let Some(slider) = param_slider_area(layout.params, index) else {
            return false;
        };
        if mouse.column < slider.x || mouse.column >= slider.x.saturating_add(slider.width) {
            return false;
        }

        self.active_panel = Panel::Params;
        self.selected_param = index;
        let mut start_ratio = self
            .selected_slot()
            .map(|slot| slot.param_ratio(index))
            .unwrap_or(0.0);
        if !mouse.modifiers.contains(KeyModifiers::SHIFT) {
            start_ratio = slider_ratio_at(slider, mouse.column);
            self.set_param_ratio(self.selected_slot, index, start_ratio);
        }
        self.param_drag = Some(ParamDrag {
            slot: self.selected_slot,
            param: index,
            slider,
            start_col: mouse.column,
            start_ratio,
        });
        true
    }

    fn drag_param_at(&mut self, mouse: MouseEvent) {
        let Some(drag) = self.param_drag else {
            return;
        };
        let (ratio, next_drag) =
            drag.updated(mouse.column, mouse.modifiers.contains(KeyModifiers::SHIFT));
        self.set_param_ratio(drag.slot, drag.param, ratio);
        self.param_drag = Some(next_drag);
    }

    fn scroll_at(&mut self, column: u16, row: u16, delta: isize, layout: AppLayout, fine: bool) {
        if rect_contains(layout.input, column, row) {
            self.active_panel = Panel::Input;
            self.move_selection(delta);
        } else if rect_contains(layout.output, column, row) {
            self.active_panel = Panel::Output;
            self.move_selection(delta);
        } else if rect_contains(layout.chain, column, row) {
            self.active_panel = Panel::Chain;
            self.move_selection(delta);
        } else if rect_contains(layout.params, column, row) {
            self.active_panel = Panel::Params;
            let scale = if fine { 0.2 } else { 1.0 };
            let direction = if delta.is_negative() { scale } else { -scale };
            self.adjust_active_param(direction);
        }
    }

    fn selected_slot(&self) -> Option<&ProcessorSlot> {
        self.slots.get(self.selected_slot)
    }
    fn toggle_stream(&mut self) -> Result<()> {
        if self.session.is_some() {
            self.stop_stream();
            return Ok(());
        }

        self.start_stream()
    }

    fn start_stream(&mut self) -> Result<()> {
        if self.input_devices.is_empty() || self.output_devices.is_empty() {
            self.status = "No input/output devices available".to_string();
            return Ok(());
        }

        self.processor = build_shared_processor(&self.slots);
        let input_device = select_input_device(&self.host, self.selected_input)?;
        let output_device = select_output_device(&self.host, self.selected_output)?;
        let session = start_live_audio(input_device, output_device, Arc::clone(&self.processor))?;
        session.play()?;
        self.status = "Running".to_string();
        self.session = Some(session);
        Ok(())
    }

    fn restart_stream(&mut self) -> Result<()> {
        let was_running = self.session.is_some();
        self.stop_stream();
        if was_running {
            self.start_stream()?;
            self.status = "Restarted".to_string();
        } else {
            self.processor = build_shared_processor(&self.slots);
            self.status = "Ready".to_string();
        }
        Ok(())
    }

    fn stop_stream(&mut self) {
        if let Some(session) = self.session.take() {
            if let Err(error) = session.stop() {
                self.status = format!("Stop failed: {error}");
                return;
            }
        }
        self.status = "Stopped".to_string();
    }

    fn toggle_view(&mut self) {
        let next = match self.view {
            AppView::Live => AppView::Tuner,
            AppView::Tuner => AppView::Live,
        };
        self.set_view(next);
    }

    fn set_view(&mut self, view: AppView) {
        if view == self.view {
            return;
        }
        match view {
            AppView::Live => {
                self.stop_tuner();
                self.view = AppView::Live;
                self.status = "Stopped".to_string();
            }
            AppView::Tuner => {
                self.stop_stream();
                self.view = AppView::Tuner;
                self.start_tuner();
            }
        }
    }

    fn start_tuner(&mut self) {
        if self.input_devices.is_empty() {
            self.status = "No input devices available".to_string();
            return;
        }
        let input_device = match select_input_device(&self.host, self.selected_input) {
            Ok(device) => device,
            Err(error) => {
                self.status = format!("Tuner input failed: {error}");
                return;
            }
        };
        let session = match start_tuner_audio(input_device) {
            Ok(session) => session,
            Err(error) => {
                self.status = format!("Tuner start failed: {error}");
                return;
            }
        };
        if let Err(error) = session.play() {
            self.status = format!("Tuner play failed: {error}");
            return;
        }
        let sample_rate = session.input_config.sample_rate().0 as f32;
        self.pitch_detector = Some(PitchDetector::new(sample_rate, 55.0, 1_760.0));
        self.pitch_history.clear();
        self.tuner_reading = None;
        self.last_tuner_analysis = Instant::now();
        self.status = "Tuner running".to_string();
        self.tuner_session = Some(session);
    }

    fn stop_tuner(&mut self) {
        if let Some(session) = self.tuner_session.take() {
            if let Err(error) = session.stop() {
                self.status = format!("Tuner stop failed: {error}");
            }
        }
        self.pitch_detector = None;
        self.pitch_history.clear();
        self.tuner_reading = None;
    }

    fn toggle_tuner(&mut self) {
        if self.tuner_session.is_some() {
            self.stop_tuner();
            self.status = "Tuner stopped".to_string();
        } else {
            self.start_tuner();
        }
    }

    fn restart_tuner(&mut self) {
        self.stop_tuner();
        self.start_tuner();
    }

    fn move_tuner_input(&mut self, delta: isize) {
        let next = moved_index(self.selected_input, self.input_devices.len(), delta);
        if next != self.selected_input {
            self.selected_input = next;
            self.restart_tuner();
        }
    }

    fn update_tuner(&mut self) {
        const ANALYSIS_INTERVAL: Duration = Duration::from_millis(50);
        const READING_TIMEOUT: Duration = Duration::from_millis(500);

        if self.view != AppView::Tuner || self.last_tuner_analysis.elapsed() < ANALYSIS_INTERVAL {
            return;
        }
        self.last_tuner_analysis = Instant::now();

        let estimate = self
            .tuner_session
            .as_ref()
            .zip(self.pitch_detector.as_ref())
            .and_then(|(session, detector)| {
                let samples = session.samples();
                let sample_rate = session.input_config.sample_rate().0 as f32;
                let window_len = ((sample_rate / 55.0) * 3.0).ceil() as usize;
                if samples.len() < window_len {
                    return None;
                }
                detector.estimate(&samples[samples.len() - window_len..])
            });

        if let Some(estimate) = estimate {
            if self.pitch_history.len() == 5 {
                self.pitch_history.pop_front();
            }
            self.pitch_history.push_back(estimate);
            self.tuner_reading =
                median_estimate(&self.pitch_history).map(|estimate| TunerReading {
                    estimate,
                    updated_at: Instant::now(),
                });
        } else if self
            .tuner_reading
            .is_some_and(|reading| reading.updated_at.elapsed() >= READING_TIMEOUT)
        {
            self.tuner_reading = None;
            self.pitch_history.clear();
        }
    }

    fn stop_all(&mut self) {
        self.stop_tuner();
        self.stop_stream();
    }

    fn activate_selection(&mut self) -> Result<()> {
        match self.active_panel {
            Panel::Input | Panel::Output => {
                if self.session.is_some() {
                    self.restart_stream()?;
                }
            }
            Panel::Chain | Panel::Params => self.rebuild_processor(),
        }
        Ok(())
    }

    fn handle_effect_picker_key(&mut self, key: KeyEvent) {
        let selected = self.effect_picker.unwrap_or(0);
        match key.code {
            KeyCode::Esc => self.effect_picker = None,
            KeyCode::Up => self.effect_picker = Some(moved_index(selected, 4, -1)),
            KeyCode::Down => self.effect_picker = Some(moved_index(selected, 4, 1)),
            KeyCode::Enter => {
                self.add_slot(ProcessorKind::ALL[selected]);
                self.effect_picker = None;
            }
            KeyCode::Char(value @ '1'..='4') => {
                let index = usize::from(value as u8 - b'1');
                self.add_slot(ProcessorKind::ALL[index]);
                self.effect_picker = None;
            }
            _ => {}
        }
    }

    fn handle_chain_action(&mut self, action: ChainAction) {
        self.active_panel = Panel::Chain;
        match action {
            ChainAction::Select(index) => {
                self.selected_slot = index;
                self.selected_param = 0;
            }
            ChainAction::Toggle(index) => {
                self.selected_slot = index;
                self.selected_param = clamped_param(self.selected_slot(), self.selected_param);
                self.toggle_slot();
            }
            ChainAction::MoveUp(index) => {
                self.selected_slot = index;
                self.selected_param = clamped_param(self.selected_slot(), self.selected_param);
                move_slot(&mut self.slots, &mut self.selected_slot, -1);
                self.rebuild_processor();
            }
            ChainAction::MoveDown(index) => {
                self.selected_slot = index;
                self.selected_param = clamped_param(self.selected_slot(), self.selected_param);
                move_slot(&mut self.slots, &mut self.selected_slot, 1);
                self.rebuild_processor();
            }
            ChainAction::Delete(index) => {
                self.selected_slot = index;
                self.delete_slot();
            }
            ChainAction::Add => self.effect_picker = Some(0),
        }
    }

    fn add_slot(&mut self, kind: ProcessorKind) {
        self.slots.push(ProcessorSlot::new(kind));
        self.selected_slot = self.slots.len().saturating_sub(1);
        self.selected_param = 0;
        self.rebuild_processor();
    }

    fn toggle_slot(&mut self) {
        if let Some(slot) = self.slots.get_mut(self.selected_slot) {
            slot.enabled = !slot.enabled;
            self.rebuild_processor();
        }
    }

    fn delete_slot(&mut self) {
        if self.slots.is_empty() {
            return;
        }

        self.slots.remove(self.selected_slot);
        self.selected_slot = self.selected_slot.min(self.slots.len().saturating_sub(1));
        self.selected_param = clamped_param(self.selected_slot(), self.selected_param);
        self.rebuild_processor();
    }

    fn toggle_slot_kind(&mut self) {
        if let Some(slot) = self.slots.get_mut(self.selected_slot) {
            slot.kind = match slot.kind {
                ProcessorKind::Gain => ProcessorKind::LowPass,
                ProcessorKind::LowPass => ProcessorKind::Delay,
                ProcessorKind::Delay => ProcessorKind::Distortion,
                ProcessorKind::Distortion => ProcessorKind::Gain,
            };
            self.selected_param = self
                .selected_param
                .min(slot.param_count().saturating_sub(1));
            self.rebuild_processor();
        }
    }

    fn move_param(&mut self, delta: isize) {
        if let Some(slot) = self.slots.get(self.selected_slot) {
            self.selected_param = moved_index(self.selected_param, slot.param_count(), delta);
        }
    }

    fn move_selection(&mut self, delta: isize) {
        match self.active_panel {
            Panel::Input => {
                self.selected_input =
                    moved_index(self.selected_input, self.input_devices.len(), delta);
            }
            Panel::Output => {
                self.selected_output =
                    moved_index(self.selected_output, self.output_devices.len(), delta);
            }
            Panel::Chain | Panel::Params => {
                self.selected_slot = moved_index(self.selected_slot, self.slots.len(), delta);
                self.selected_param = 0;
            }
        }
    }

    fn adjust_active_param(&mut self, direction: f32) {
        if !matches!(self.active_panel, Panel::Params | Panel::Chain) {
            return;
        }

        if let Some(slot) = self.slots.get_mut(self.selected_slot) {
            slot.adjust_param(self.selected_param, direction);
            self.rebuild_processor();
        }
    }

    fn set_param_ratio(&mut self, slot_index: usize, param: usize, ratio: f32) {
        if let Some(slot) = self.slots.get_mut(slot_index) {
            slot.set_param_ratio(param, ratio);
            self.rebuild_processor();
        }
    }

    fn rebuild_processor(&mut self) {
        let next = LiveChain::from_slots(&self.slots);
        let updated = if let Ok(mut processor) = self.processor.lock() {
            *processor = Box::new(next);
            true
        } else {
            false
        };

        if !updated {
            self.processor = build_shared_processor(&self.slots);
        }
        self.status = if self.session.is_some() {
            "Running - chain updated".to_string()
        } else {
            "Ready".to_string()
        };
    }

    fn input_stats(&self) -> AudioStats {
        self.session
            .as_ref()
            .map(LiveAudioSession::input_stats)
            .unwrap_or_default()
    }

    fn output_stats(&self) -> AudioStats {
        self.session
            .as_ref()
            .map(LiveAudioSession::output_stats)
            .unwrap_or_default()
    }
}

fn moved_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }

    let last = len - 1;
    if delta.is_negative() {
        current.saturating_sub(delta.unsigned_abs()).min(last)
    } else {
        current.saturating_add(delta as usize).min(last)
    }
}

fn median_estimate(history: &VecDeque<PitchEstimate>) -> Option<PitchEstimate> {
    let mut estimates = history.iter().copied().collect::<Vec<_>>();
    estimates.sort_by(|left, right| left.frequency_hz.total_cmp(&right.frequency_hz));
    estimates.get(estimates.len() / 2).copied()
}

fn view_at(area: Rect, column: u16, row: u16) -> Option<AppView> {
    if row != area.y.saturating_add(1) {
        return None;
    }
    let offset = column.saturating_sub(area.x);
    if (1..=6).contains(&offset) {
        Some(AppView::Live)
    } else if (8..=14).contains(&offset) {
        Some(AppView::Tuner)
    } else {
        None
    }
}

fn key_adjustment(modifiers: KeyModifiers) -> f32 {
    if modifiers.contains(KeyModifiers::SHIFT) {
        0.2
    } else {
        1.0
    }
}

fn rect_contains(area: Rect, column: u16, row: u16) -> bool {
    let right = u32::from(area.x) + u32::from(area.width);
    let bottom = u32::from(area.y) + u32::from(area.height);
    u32::from(column) >= u32::from(area.x)
        && u32::from(column) < right
        && u32::from(row) >= u32::from(area.y)
        && u32::from(row) < bottom
}

fn inner_area(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

fn effect_picker_area(area: Rect) -> Rect {
    let width = area.width.min(30);
    let height = area.height.min(6);
    Rect::new(
        area.x.saturating_add(area.width.saturating_sub(width) / 2),
        area.y
            .saturating_add(area.height.saturating_sub(height) / 2),
        width,
        height,
    )
}

fn list_window_start(selected: usize, len: usize, visible_rows: usize) -> usize {
    if visible_rows == 0 || len <= visible_rows {
        return 0;
    }
    selected
        .saturating_add(1)
        .saturating_sub(visible_rows)
        .min(len - visible_rows)
}

fn visible_list_index_at(
    area: Rect,
    column: u16,
    row: u16,
    len: usize,
    selected: usize,
) -> Option<usize> {
    let area = inner_area(area);
    if len == 0 || !rect_contains(area, column, row) {
        return None;
    }

    let start = list_window_start(selected, len, usize::from(area.height));
    let index = start + usize::from(row.saturating_sub(area.y));
    (index < len).then_some(index)
}

fn param_index_at(area: Rect, row: u16, slot: Option<&ProcessorSlot>) -> Option<usize> {
    let slot = slot?;
    let content = inner_area(area);
    let first_param_row = content.y.saturating_add(1);
    if row < first_param_row {
        return None;
    }

    let index = usize::from(row - first_param_row);
    (index < slot.param_count()).then_some(index)
}

fn clamped_param(slot: Option<&ProcessorSlot>, selected_param: usize) -> usize {
    slot.map(|slot| selected_param.min(slot.param_count().saturating_sub(1)))
        .unwrap_or(0)
}

fn param_slider_area(area: Rect, param: usize) -> Option<Rect> {
    let content = inner_area(area);
    if content.width <= 26 || param >= usize::from(content.height.saturating_sub(1)) {
        return None;
    }

    Some(Rect::new(
        content.x.saturating_add(13),
        content
            .y
            .saturating_add(1)
            .saturating_add(u16::try_from(param).unwrap_or(u16::MAX)),
        content.width - 26,
        1,
    ))
}

fn slider_ratio_at(area: Rect, column: u16) -> f32 {
    if area.width <= 1 {
        return 0.0;
    }

    let offset = column.saturating_sub(area.x).min(area.width - 1);
    f32::from(offset) / f32::from(area.width - 1)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChainAction {
    Select(usize),
    Toggle(usize),
    MoveUp(usize),
    MoveDown(usize),
    Delete(usize),
    Add,
}

fn chain_window(selected: usize, slot_count: usize, capacity: usize) -> (usize, usize) {
    let start = list_window_start(selected, slot_count, capacity);
    let count = slot_count.saturating_sub(start).min(capacity);
    (start, count)
}

fn chain_action_at(
    area: Rect,
    column: u16,
    row: u16,
    slot_count: usize,
    selected: usize,
) -> Option<ChainAction> {
    let content = inner_area(area);
    if !rect_contains(content, column, row) {
        return None;
    }

    let capacity = usize::from(content.height.saturating_sub(1));
    let (start, visible_count) = chain_window(selected, slot_count, capacity);
    let visible_index = usize::from(row - content.y);
    if visible_index == visible_count {
        return Some(ChainAction::Add);
    }
    if visible_index > visible_count {
        return None;
    }
    let index = start + visible_index;

    let right = content.x.saturating_add(content.width);
    let actions_start = right.saturating_sub(11);
    if column >= right.saturating_sub(3) {
        Some(ChainAction::Delete(index))
    } else if column >= right.saturating_sub(7) {
        Some(ChainAction::MoveDown(index))
    } else if column >= actions_start {
        Some(ChainAction::MoveUp(index))
    } else if column >= content.x.saturating_add(4) && column < content.x.saturating_add(7) {
        Some(ChainAction::Toggle(index))
    } else {
        Some(ChainAction::Select(index))
    }
}

fn move_slot(slots: &mut [ProcessorSlot], selected: &mut usize, delta: isize) {
    if slots.is_empty() {
        return;
    }

    let target = moved_index(*selected, slots.len(), delta);
    if target != *selected {
        slots.swap(*selected, target);
        *selected = target;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessorKind {
    Gain,
    LowPass,
    Delay,
    Distortion,
}

impl ProcessorKind {
    const ALL: [Self; 4] = [Self::Gain, Self::LowPass, Self::Delay, Self::Distortion];

    fn label(self) -> &'static str {
        match self {
            Self::Gain => "Gain",
            Self::LowPass => "LowPass",
            Self::Delay => "Delay",
            Self::Distortion => "Distortion",
        }
    }
}

fn effect_kind_at(area: Rect, column: u16, row: u16) -> Option<ProcessorKind> {
    let content = inner_area(area);
    if !rect_contains(content, column, row) {
        return None;
    }

    ProcessorKind::ALL
        .get(usize::from(row - content.y))
        .copied()
}

#[derive(Debug, Clone)]
struct ProcessorSlot {
    kind: ProcessorKind,
    enabled: bool,
    gain: f32,
    cutoff_hz: f32,
    delay_ms: f32,
    feedback: f32,
    mix: f32,
    drive: f32,
}

impl ProcessorSlot {
    fn new(kind: ProcessorKind) -> Self {
        Self {
            kind,
            enabled: true,
            gain: 1.0,
            cutoff_hz: 2_000.0,
            delay_ms: 250.0,
            feedback: 0.25,
            mix: 0.35,
            drive: 3.0,
        }
    }

    fn gain() -> Self {
        Self::new(ProcessorKind::Gain)
    }

    fn param_count(&self) -> usize {
        match self.kind {
            ProcessorKind::Gain => 1,
            ProcessorKind::LowPass => 1,
            ProcessorKind::Delay => 3,
            ProcessorKind::Distortion => 2,
        }
    }

    fn adjust_param(&mut self, param: usize, direction: f32) {
        match self.kind {
            ProcessorKind::Gain => {
                self.gain = (self.gain + direction * 0.05).clamp(0.0, 4.0);
            }
            ProcessorKind::LowPass => {
                let step = if self.cutoff_hz < 1_000.0 {
                    25.0
                } else {
                    250.0
                };
                self.cutoff_hz = (self.cutoff_hz + direction * step).clamp(20.0, 20_000.0);
            }
            ProcessorKind::Delay => match param {
                0 => self.delay_ms = (self.delay_ms + direction * 10.0).clamp(0.0, 2_000.0),
                1 => self.feedback = (self.feedback + direction * 0.025).clamp(0.0, 0.95),
                _ => self.mix = (self.mix + direction * 0.025).clamp(0.0, 1.0),
            },
            ProcessorKind::Distortion => match param {
                0 => self.drive = (self.drive + direction * 0.25).clamp(1.0, 20.0),
                _ => self.mix = (self.mix + direction * 0.025).clamp(0.0, 1.0),
            },
        }
    }

    fn set_param_ratio(&mut self, param: usize, ratio: f32) {
        let ratio = ratio.clamp(0.0, 1.0);
        match self.kind {
            ProcessorKind::Gain => self.gain = ratio * 4.0,
            ProcessorKind::LowPass => {
                self.cutoff_hz = 20.0 * (1_000.0_f32).powf(ratio);
            }
            ProcessorKind::Delay => match param {
                0 => self.delay_ms = ratio * 2_000.0,
                1 => self.feedback = ratio * 0.95,
                _ => self.mix = ratio,
            },
            ProcessorKind::Distortion => match param {
                0 => self.drive = 1.0 + ratio * 19.0,
                _ => self.mix = ratio,
            },
        }
    }

    fn param_ratio(&self, param: usize) -> f32 {
        match self.kind {
            ProcessorKind::Gain => self.gain / 4.0,
            ProcessorKind::LowPass => (self.cutoff_hz / 20.0).ln() / 1_000.0_f32.ln(),
            ProcessorKind::Delay => match param {
                0 => self.delay_ms / 2_000.0,
                1 => self.feedback / 0.95,
                _ => self.mix,
            },
            ProcessorKind::Distortion => match param {
                0 => (self.drive - 1.0) / 19.0,
                _ => self.mix,
            },
        }
        .clamp(0.0, 1.0)
    }
}

struct LiveChain {
    processors: Vec<(bool, ProcessorNode)>,
}

impl LiveChain {
    fn from_slots(slots: &[ProcessorSlot]) -> Self {
        let processors = slots
            .iter()
            .map(|slot| {
                let node = match slot.kind {
                    ProcessorKind::Gain => {
                        let mut processor = GainProcessor::new();
                        processor.set_gain(slot.gain);
                        ProcessorNode::Gain(processor)
                    }
                    ProcessorKind::LowPass => {
                        ProcessorNode::LowPass(OnePoleLowPass::new(slot.cutoff_hz))
                    }
                    ProcessorKind::Delay => ProcessorNode::Delay(DelayProcessor::new(
                        slot.delay_ms,
                        slot.feedback,
                        slot.mix,
                    )),
                    ProcessorKind::Distortion => {
                        ProcessorNode::Distortion(DistortionProcessor::new(slot.drive, slot.mix))
                    }
                };
                (slot.enabled, node)
            })
            .collect();

        Self { processors }
    }
}

impl AudioProcessor for LiveChain {
    fn prepare(&mut self, sample_rate: f32, max_block_size: usize) {
        for (enabled, processor) in &mut self.processors {
            if *enabled {
                processor.prepare(sample_rate, max_block_size);
            }
        }
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        for (enabled, processor) in &mut self.processors {
            if *enabled {
                processor.process(ctx);
            }
        }
    }

    fn reset(&mut self) {
        for (_, processor) in &mut self.processors {
            processor.reset();
        }
    }
}

enum ProcessorNode {
    Gain(GainProcessor),
    LowPass(OnePoleLowPass),
    Delay(DelayProcessor),
    Distortion(DistortionProcessor),
}

impl AudioProcessor for ProcessorNode {
    fn prepare(&mut self, sample_rate: f32, max_block_size: usize) {
        match self {
            Self::Gain(processor) => processor.prepare(sample_rate, max_block_size),
            Self::LowPass(processor) => processor.prepare(sample_rate, max_block_size),
            Self::Delay(processor) => processor.prepare(sample_rate, max_block_size),
            Self::Distortion(processor) => processor.prepare(sample_rate, max_block_size),
        }
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        match self {
            Self::Gain(processor) => processor.process(ctx),
            Self::LowPass(processor) => processor.process(ctx),
            Self::Delay(processor) => processor.process(ctx),
            Self::Distortion(processor) => processor.process(ctx),
        }
    }

    fn reset(&mut self) {
        match self {
            Self::Gain(processor) => processor.reset(),
            Self::LowPass(processor) => processor.reset(),
            Self::Delay(processor) => processor.reset(),
            Self::Distortion(processor) => processor.reset(),
        }
    }
}

fn build_shared_processor(slots: &[ProcessorSlot]) -> SharedProcessor {
    Arc::new(Mutex::new(Box::new(LiveChain::from_slots(slots))))
}

fn note_label(midi_note: i16) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let name = NAMES[midi_note.rem_euclid(12) as usize];
    let octave = midi_note.div_euclid(12) - 1;
    format!("{name}{octave}")
}

#[derive(Debug, Clone, Copy)]
struct GuitarTarget {
    index: usize,
    label: &'static str,
    frequency_hz: f32,
    cents: f32,
}

fn guitar_target(frequency_hz: f32) -> GuitarTarget {
    const STRINGS: [(&str, f32); 6] = [
        ("E2", 82.406_89),
        ("A2", 110.0),
        ("D3", 146.832_38),
        ("G3", 195.997_71),
        ("B3", 246.941_65),
        ("E4", 329.627_56),
    ];
    STRINGS
        .iter()
        .enumerate()
        .map(|(index, (label, target_hz))| GuitarTarget {
            index,
            label,
            frequency_hz: *target_hz,
            cents: 1_200.0 * (frequency_hz / target_hz).log2(),
        })
        .min_by(|left, right| left.cents.abs().total_cmp(&right.cents.abs()))
        .expect("standard guitar tuning is not empty")
}

fn draw(frame: &mut Frame<'_>, app: &App) {
    if app.view == AppView::Tuner {
        draw_tuner_view(frame, app);
        return;
    }
    let layout = AppLayout::new(frame.size());

    draw_status(frame, layout.status, app);
    draw_devices(
        frame,
        layout.input,
        "Input",
        &app.input_devices,
        app.selected_input,
        app.active_panel == Panel::Input,
    );
    draw_devices(
        frame,
        layout.output,
        "Output",
        &app.output_devices,
        app.selected_output,
        app.active_panel == Panel::Output,
    );
    draw_chain(frame, layout.chain, app);
    draw_params(frame, layout.params, app);
    draw_meters(frame, layout.meters, app);
    draw_help(frame, layout.help);
    if let Some(selected) = app.effect_picker {
        draw_effect_picker(frame, effect_picker_area(frame.size()), selected);
    }
}

fn draw_status(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let status = if let Some(session) = &app.tuner_session {
        format!(
            "{} | {} | {} Hz",
            app.status,
            session.input_name,
            session.input_config.sample_rate().0
        )
    } else if let Some(session) = &app.session {
        format!(
            "{} | {} -> {} | {} Hz",
            app.status,
            session.input_name,
            session.output_name,
            session.output_config.sample_rate().0
        )
    } else {
        format!("{} | stream stopped", app.status)
    };
    let live_style = if app.view == AppView::Live {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let tuner_style = if app.view == AppView::Tuner {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let line = Line::from(vec![
        Span::styled("[Live]", live_style),
        Span::raw(" "),
        Span::styled("[Tuner]", tuner_style),
        Span::raw(format!("  {status}")),
    ]);
    frame.render_widget(
        Paragraph::new(line).block(
            Block::default()
                .borders(Borders::ALL)
                .title("crabjuice live"),
        ),
        area,
    );
}

fn draw_tuner_view(frame: &mut Frame<'_>, app: &App) {
    let layout = TunerLayout::new(frame.size());
    draw_status(frame, layout.status, app);
    draw_devices(
        frame,
        layout.input,
        "Tuner input",
        &app.input_devices,
        app.selected_input,
        true,
    );
    let mode_line = Line::from(vec![
        Span::raw("Mode: "),
        Span::styled(
            app.tuner_mode.label(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  (m or click to toggle)"),
    ]);
    frame.render_widget(
        Paragraph::new(mode_line).block(Block::default().borders(Borders::ALL).title("Mode")),
        layout.mode,
    );
    draw_tuner_panel(frame, layout.tuner, app.tuner_mode, app.tuner_reading);
    let stats = app
        .tuner_session
        .as_ref()
        .map(TunerSession::input_stats)
        .unwrap_or_default();
    render_meter(frame, layout.meter, "Input level", stats);
    frame.render_widget(
        Paragraph::new("↑/↓ input | m mode | Space capture | r retry | Shift+T Live | q quit")
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: true }),
        layout.help,
    );
}

fn draw_tuner_panel(
    frame: &mut Frame<'_>,
    area: Rect,
    mode: TunerMode,
    reading: Option<TunerReading>,
) {
    let Some(reading) = reading else {
        frame.render_widget(
            Paragraph::new("No signal")
                .block(Block::default().borders(Borders::ALL).title("Tuner")),
            area,
        );
        return;
    };

    let estimate = reading.estimate;
    let (label, cents, guitar) = match mode {
        TunerMode::Chromatic => (note_label(estimate.midi_note), estimate.cents, None),
        TunerMode::Guitar => {
            let target = guitar_target(estimate.frequency_hz);
            (target.label.to_string(), target.cents, Some(target))
        }
    };
    let color = if cents.abs() <= 5.0 {
        Color::Green
    } else {
        Color::Yellow
    };
    let bar_width = usize::from(inner_area(area).width).clamp(11, 61);
    let mut lines = vec![
        Line::from(Span::styled(
            label,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )),
        Line::from(format!(
            "{:.2} Hz   {:+.1} cents   confidence {:.0}%",
            estimate.frequency_hz,
            cents,
            estimate.confidence * 100.0
        )),
        Line::from(Span::styled(
            tuning_bar(cents, bar_width),
            Style::default().fg(color),
        )),
        Line::from("-50 cents                 0                 +50 cents"),
    ];
    if let Some(target) = guitar {
        let labels = ["E2", "A2", "D3", "G3", "B3", "E4"];
        let strings = labels
            .iter()
            .enumerate()
            .flat_map(|(index, label)| {
                let style = if index == target.index {
                    Style::default().fg(color).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                [Span::styled(format!(" {label} "), style), Span::raw(" ")]
            })
            .collect::<Vec<_>>();
        lines.push(Line::from(strings));
        lines.push(Line::from(format!("Target {:.2} Hz", target.frequency_hz)));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title("Tuner"))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn tuning_bar(cents: f32, width: usize) -> String {
    let width = width.max(3);
    let position =
        (((cents.clamp(-50.0, 50.0) + 50.0) / 100.0) * (width - 1) as f32).round() as usize;
    let center = width / 2;
    (0..width)
        .map(|index| {
            if index == position {
                '▲'
            } else if index == center {
                '│'
            } else {
                '─'
            }
        })
        .collect()
}

fn draw_devices(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    devices: &[DeviceInfo],
    selected: usize,
    focused: bool,
) {
    let items = if devices.is_empty() {
        vec![ListItem::new("No devices")]
    } else {
        let visible_rows = usize::from(inner_area(area).height);
        let start = list_window_start(selected, devices.len(), visible_rows);
        devices
            .iter()
            .skip(start)
            .take(visible_rows)
            .map(|device| {
                let marker = if device.index == selected { "> " } else { "  " };
                let default = if device.is_default { " [default]" } else { "" };
                ListItem::new(format!(
                    "{marker}{}: {}{}",
                    device.index, device.name, default
                ))
            })
            .collect()
    };
    frame.render_widget(List::new(items).block(panel_block(title, focused)), area);
}

fn draw_chain(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let items = if app.slots.is_empty() {
        vec![ListItem::new("+ Add effect")]
    } else {
        let capacity = usize::from(inner_area(area).height.saturating_sub(1));
        let (start, visible_count) = chain_window(app.selected_slot, app.slots.len(), capacity);
        let mut items: Vec<_> = app
            .slots
            .iter()
            .enumerate()
            .skip(start)
            .take(visible_count)
            .map(|(index, slot)| {
                let selected = if index == app.selected_slot {
                    "> "
                } else {
                    "  "
                };
                let enabled = if slot.enabled { "●" } else { "○" };
                let display_index = index + 1;
                let prefix = format!(
                    "{selected}{display_index:>2} {enabled} {:<10}",
                    slot.kind.label()
                );
                let prefix_width = usize::from(inner_area(area).width.saturating_sub(11));
                let line = format!("{prefix:<prefix_width$}[^] [v] [x]");
                let style = if slot.enabled {
                    Style::default()
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                ListItem::new(line).style(style)
            })
            .collect();
        items.push(ListItem::new("+ Add effect"));
        items
    };
    frame.render_widget(
        List::new(items).block(panel_block("Chain", app.active_panel == Panel::Chain)),
        area,
    );
}

fn draw_params(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let lines = if let Some(slot) = app.slots.get(app.selected_slot) {
        let mut lines = vec![Line::from(vec![
            Span::styled("Type: ", Style::default().fg(Color::Gray)),
            Span::raw(slot.kind.label()),
            Span::raw("  "),
            Span::styled("Enabled: ", Style::default().fg(Color::Gray)),
            Span::raw(if slot.enabled { "yes" } else { "no" }),
        ])];
        lines.extend(param_lines(slot, app.selected_param, area));
        lines.extend([
            Line::from("Click/drag slider; Shift-drag adjusts finely."),
            Line::from("Wheel or arrows adjust; [/] selects parameter."),
        ]);
        lines
    } else {
        vec![Line::from("No selected slot.")]
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel_block("Params", app.active_panel == Panel::Params))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn param_lines(slot: &ProcessorSlot, selected_param: usize, area: Rect) -> Vec<Line<'static>> {
    match slot.kind {
        ProcessorKind::Gain => vec![param_line(
            0,
            selected_param,
            "Gain",
            format!("{:.2}", slot.gain),
            slot.param_ratio(0),
            area,
        )],
        ProcessorKind::LowPass => vec![param_line(
            0,
            selected_param,
            "Cutoff",
            format!("{:.0} Hz", slot.cutoff_hz),
            slot.param_ratio(0),
            area,
        )],
        ProcessorKind::Delay => vec![
            param_line(
                0,
                selected_param,
                "Delay",
                format!("{:.0} ms", slot.delay_ms),
                slot.param_ratio(0),
                area,
            ),
            param_line(
                1,
                selected_param,
                "Feedback",
                format!("{:.2}", slot.feedback),
                slot.param_ratio(1),
                area,
            ),
            param_line(
                2,
                selected_param,
                "Mix",
                format!("{:.0}%", slot.mix * 100.0),
                slot.param_ratio(2),
                area,
            ),
        ],
        ProcessorKind::Distortion => vec![
            param_line(
                0,
                selected_param,
                "Drive",
                format!("{:.2}", slot.drive),
                slot.param_ratio(0),
                area,
            ),
            param_line(
                1,
                selected_param,
                "Mix",
                format!("{:.0}%", slot.mix * 100.0),
                slot.param_ratio(1),
                area,
            ),
        ],
    }
}

fn param_line(
    index: usize,
    selected_param: usize,
    name: &'static str,
    value: String,
    ratio: f32,
    area: Rect,
) -> Line<'static> {
    let selected = index == selected_param;
    let marker = if selected { "> " } else { "  " };
    let Some(slider) = param_slider_area(area, index) else {
        return Line::from(format!("{marker}{name}: {value}"));
    };
    let width = usize::from(slider.width);
    let filled = ((ratio.clamp(0.0, 1.0) * width as f32).round() as usize).min(width);
    let marker_style = if selected {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::styled(marker, marker_style),
        Span::styled(format!("{name:<9} ["), marker_style),
        Span::styled("█".repeat(filled), Style::default().fg(Color::Cyan)),
        Span::styled(
            "─".repeat(width - filled),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(format!("] {value}")),
    ])
}

fn draw_effect_picker(frame: &mut Frame<'_>, area: Rect, selected: usize) {
    let items = ProcessorKind::ALL
        .iter()
        .enumerate()
        .map(|(index, kind)| {
            let marker = if index == selected { "> " } else { "  " };
            ListItem::new(format!("{marker}{}. {}", index + 1, kind.label()))
        })
        .collect::<Vec<_>>();
    frame.render_widget(Clear, area);
    frame.render_widget(
        List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Add effect (1-4)"),
        ),
        area,
    );
}

fn draw_meters(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);
    render_meter(frame, chunks[0], "Input", app.input_stats());
    render_meter(frame, chunks[1], "Output", app.output_stats());
}

fn render_meter(frame: &mut Frame<'_>, area: Rect, title: &str, stats: AudioStats) {
    let label = format!("peak {:.2}  rms {:.2}", stats.peak, stats.rms);
    frame.render_widget(
        Gauge::default()
            .block(Block::default().borders(Borders::ALL).title(title))
            .gauge_style(Style::default().fg(Color::Green))
            .ratio(stats.peak.clamp(0.0, 1.0) as f64)
            .label(label),
        area,
    );
}

fn draw_help(frame: &mut Frame<'_>, area: Rect) {
    let text =
        "a add | chain actions | drag sliders (Shift fine) | Space stream | Shift+T tuner | q quit";
    frame.render_widget(
        Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn panel_block(title: &str, focused: bool) -> Block<'_> {
    let style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(title.to_string(), style))
        .border_style(style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;

    #[test]
    fn moved_index_clamps_to_available_range() {
        assert_eq!(moved_index(0, 3, -1), 0);
        assert_eq!(moved_index(1, 3, 1), 2);
        assert_eq!(moved_index(2, 3, 1), 2);
        assert_eq!(moved_index(0, 0, 1), 0);
    }

    #[test]
    fn live_chain_keeps_slot_count_and_processes_enabled_slots() {
        let slots = vec![
            ProcessorSlot {
                kind: ProcessorKind::Gain,
                enabled: true,
                gain: 0.5,
                ..ProcessorSlot::gain()
            },
            ProcessorSlot {
                kind: ProcessorKind::Gain,
                enabled: false,
                gain: 0.0,
                ..ProcessorSlot::gain()
            },
        ];
        let chain = LiveChain::from_slots(&slots);

        assert_eq!(chain.processors.len(), 2);
    }

    #[test]
    fn slider_ratio_uses_the_full_control_width_and_clamps() {
        let area = Rect::new(10, 4, 11, 1);

        assert_eq!(slider_ratio_at(area, 5), 0.0);
        assert_eq!(slider_ratio_at(area, 10), 0.0);
        assert_eq!(slider_ratio_at(area, 15), 0.5);
        assert_eq!(slider_ratio_at(area, 20), 1.0);
        assert_eq!(slider_ratio_at(area, 30), 1.0);
    }

    #[test]
    fn setting_cutoff_from_slider_uses_a_logarithmic_scale() {
        let mut slot = ProcessorSlot {
            kind: ProcessorKind::LowPass,
            ..ProcessorSlot::gain()
        };

        slot.set_param_ratio(0, 0.0);
        assert_eq!(slot.cutoff_hz, 20.0);
        slot.set_param_ratio(0, 1.0);
        assert_eq!(slot.cutoff_hz, 20_000.0);
        slot.set_param_ratio(0, 0.5);
        assert!((slot.cutoff_hz - 632.46).abs() < 0.01);
    }

    #[test]
    fn chain_hit_test_finds_slot_actions_and_add_row() {
        let area = Rect::new(0, 0, 30, 8);

        assert_eq!(
            chain_action_at(area, 5, 1, 2, 0),
            Some(ChainAction::Toggle(0))
        );
        assert_eq!(
            chain_action_at(area, 18, 1, 2, 0),
            Some(ChainAction::MoveUp(0))
        );
        assert_eq!(
            chain_action_at(area, 22, 2, 2, 0),
            Some(ChainAction::MoveDown(1))
        );
        assert_eq!(
            chain_action_at(area, 26, 2, 2, 0),
            Some(ChainAction::Delete(1))
        );
        assert_eq!(chain_action_at(area, 4, 3, 2, 0), Some(ChainAction::Add));
    }

    #[test]
    fn moving_a_slot_keeps_the_moved_effect_selected() {
        let mut slots = vec![
            ProcessorSlot::gain(),
            ProcessorSlot {
                kind: ProcessorKind::Delay,
                ..ProcessorSlot::gain()
            },
        ];
        let mut selected = 1;

        move_slot(&mut slots, &mut selected, -1);

        assert_eq!(selected, 0);
        assert_eq!(slots[0].kind, ProcessorKind::Delay);
    }

    #[test]
    fn effect_picker_maps_each_visible_row_to_its_kind() {
        let area = Rect::new(10, 4, 28, 6);

        assert_eq!(effect_kind_at(area, 12, 5), Some(ProcessorKind::Gain));
        assert_eq!(effect_kind_at(area, 12, 6), Some(ProcessorKind::LowPass));
        assert_eq!(effect_kind_at(area, 12, 7), Some(ProcessorKind::Delay));
        assert_eq!(effect_kind_at(area, 12, 8), Some(ProcessorKind::Distortion));
        assert_eq!(effect_kind_at(area, 12, 9), None);
    }

    #[test]
    fn parameter_slider_leaves_space_for_label_and_value() {
        let panel = Rect::new(20, 3, 60, 10);

        assert_eq!(param_slider_area(panel, 0), Some(Rect::new(34, 5, 32, 1)));
        assert_eq!(param_slider_area(panel, 2), Some(Rect::new(34, 7, 32, 1)));
    }

    #[test]
    fn processor_kind_constructor_creates_the_requested_effect() {
        let slot = ProcessorSlot::new(ProcessorKind::Distortion);

        assert_eq!(slot.kind, ProcessorKind::Distortion);
        assert!(slot.enabled);
        assert_eq!(slot.drive, 3.0);
    }

    #[test]
    fn compact_device_list_keeps_selection_visible_and_clicks_absolute_index() {
        let area = Rect::new(0, 0, 30, 5);

        assert_eq!(list_window_start(5, 8, 3), 3);
        assert_eq!(visible_list_index_at(area, 2, 1, 8, 5), Some(3));
        assert_eq!(visible_list_index_at(area, 2, 3, 8, 5), Some(5));
    }

    #[test]
    fn overflowing_chain_keeps_selected_slot_and_add_action_visible() {
        let area = Rect::new(0, 0, 30, 6);

        assert_eq!(chain_window(7, 8, 3), (5, 3));
        assert_eq!(
            chain_action_at(area, 4, 1, 8, 7),
            Some(ChainAction::Select(5))
        );
        assert_eq!(chain_action_at(area, 4, 4, 8, 7), Some(ChainAction::Add));
    }

    #[test]
    fn switching_from_coarse_to_fine_drag_keeps_value_continuous() {
        let drag = ParamDrag {
            slot: 0,
            param: 0,
            slider: Rect::new(10, 4, 11, 1),
            start_col: 10,
            start_ratio: 0.2,
        };

        let (coarse_ratio, drag) = drag.updated(15, false);
        let (fine_ratio, _) = drag.updated(16, true);

        assert_eq!(coarse_ratio, 0.5);
        assert!((fine_ratio - 0.505).abs() < f32::EPSILON);
    }

    #[test]
    fn changing_to_slot_with_fewer_params_clamps_parameter_selection() {
        let gain = ProcessorSlot::gain();

        assert_eq!(clamped_param(Some(&gain), 2), 0);
        assert_eq!(clamped_param(None, 2), 0);
    }

    #[test]
    fn shift_modifier_requests_fine_keyboard_adjustment() {
        assert_eq!(key_adjustment(KeyModifiers::NONE), 1.0);
        assert_eq!(key_adjustment(KeyModifiers::SHIFT), 0.2);
    }

    #[test]
    fn note_label_uses_scientific_pitch_notation() {
        assert_eq!(note_label(69), "A4");
        assert_eq!(note_label(40), "E2");
        assert_eq!(note_label(60), "C4");
    }

    #[test]
    fn guitar_target_selects_the_nearest_standard_string() {
        let low_e = guitar_target(82.406_89);
        let a = guitar_target(110.0);

        assert_eq!(low_e.label, "E2");
        assert!(low_e.cents.abs() < 0.01);
        assert_eq!(a.label, "A2");
        assert!(a.cents.abs() < 0.01);
    }

    #[test]
    fn tuner_tabs_map_clicks_to_the_requested_view() {
        let area = Rect::new(0, 0, 80, 24);

        assert_eq!(view_at(area, 2, 1), Some(AppView::Live));
        assert_eq!(view_at(area, 10, 1), Some(AppView::Tuner));
        assert_eq!(view_at(area, 20, 1), None);
    }

    #[test]
    fn tuning_bar_centers_and_clamps_the_pitch_marker() {
        assert_eq!(tuning_bar(0.0, 11), "─────▲─────");
        assert_eq!(tuning_bar(-80.0, 11), "▲────│─────");
        assert_eq!(tuning_bar(80.0, 11), "─────│────▲");
    }

    #[test]
    fn tuner_panel_renders_no_signal_in_a_compact_terminal() {
        let backend = TestBackend::new(60, 18);
        let mut terminal = Terminal::new(backend).expect("test terminal can be created");

        terminal
            .draw(|frame| draw_tuner_panel(frame, frame.size(), TunerMode::Chromatic, None))
            .expect("compact tuner panel should render");

        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("No signal"));
    }

    #[test]
    fn median_estimate_rejects_a_single_frequency_outlier() {
        let estimate = |frequency_hz| PitchEstimate {
            frequency_hz,
            midi_note: 69,
            cents: 0.0,
            confidence: 0.95,
        };
        let history = VecDeque::from([
            estimate(440.0),
            estimate(880.0),
            estimate(439.8),
            estimate(440.2),
            estimate(439.9),
        ]);

        let median = median_estimate(&history).expect("history is not empty");

        assert_eq!(median.frequency_hz, 440.0);
    }
}
