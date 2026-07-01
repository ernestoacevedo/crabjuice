use anyhow::{Context, Result};
use cpal::Host;
use crabjuice_audio::{AudioProcessor, ProcessContext};
use crabjuice_dsp::{DelayProcessor, DistortionProcessor, GainProcessor, OnePoleLowPass};
use crossterm::event::{self, Event, KeyCode, KeyEvent};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use real_audio::{
    default_input_index, default_output_index, input_devices, output_devices, select_input_device,
    select_output_device, start_live_audio, AudioStats, DeviceInfo, LiveAudioSession,
    SharedProcessor,
};
use std::io::{self, Stdout};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
        terminal.draw(|frame| draw(frame, &app))?;

        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if app.handle_key(key)? {
                    break;
                }
            }
        }
    }

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
        execute!(stdout, EnterAlternateScreen).context("failed to enter alternate screen")?;
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
            execute!(self.terminal.backend_mut(), LeaveAlternateScreen)
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
        })
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
            KeyCode::Tab => self.active_panel = self.active_panel.next(),
            KeyCode::BackTab => self.active_panel = self.active_panel.previous(),
            KeyCode::Char(' ') => self.toggle_stream()?,
            KeyCode::Char('r') => self.restart_stream()?,
            KeyCode::Char('a') => self.add_slot(),
            KeyCode::Char('d') => self.toggle_slot(),
            KeyCode::Char('x') => self.delete_slot(),
            KeyCode::Char('t') => self.toggle_slot_kind(),
            KeyCode::Char('[') => self.move_param(-1),
            KeyCode::Char(']') => self.move_param(1),
            KeyCode::Up => self.move_selection(-1),
            KeyCode::Down => self.move_selection(1),
            KeyCode::Left => self.adjust_active_param(-1.0),
            KeyCode::Right => self.adjust_active_param(1.0),
            KeyCode::Enter => self.activate_selection()?,
            _ => {}
        }

        Ok(false)
    }

    fn toggle_stream(&mut self) -> Result<()> {
        if self.session.is_some() {
            self.session = None;
            self.status = "Stopped".to_string();
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
        self.session = None;
        if was_running {
            self.start_stream()?;
            self.status = "Restarted".to_string();
        } else {
            self.processor = build_shared_processor(&self.slots);
            self.status = "Ready".to_string();
        }
        Ok(())
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

    fn add_slot(&mut self) {
        self.slots.push(ProcessorSlot::gain());
        self.selected_slot = self.slots.len().saturating_sub(1);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessorKind {
    Gain,
    LowPass,
    Delay,
    Distortion,
}

impl ProcessorKind {
    fn label(self) -> &'static str {
        match self {
            Self::Gain => "Gain",
            Self::LowPass => "LowPass",
            Self::Delay => "Delay",
            Self::Distortion => "Distort",
        }
    }
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
    fn gain() -> Self {
        Self {
            kind: ProcessorKind::Gain,
            enabled: true,
            gain: 1.0,
            cutoff_hz: 2_000.0,
            delay_ms: 250.0,
            feedback: 0.25,
            mix: 0.35,
            drive: 3.0,
        }
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

fn draw(frame: &mut Frame<'_>, app: &App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(5),
            Constraint::Length(3),
        ])
        .split(frame.size());
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(34),
            Constraint::Percentage(33),
            Constraint::Percentage(33),
        ])
        .split(root[1]);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(columns[2]);

    draw_status(frame, root[0], app);
    draw_devices(
        frame,
        columns[0],
        "Input",
        &app.input_devices,
        app.selected_input,
        app.active_panel == Panel::Input,
    );
    draw_devices(
        frame,
        columns[1],
        "Output",
        &app.output_devices,
        app.selected_output,
        app.active_panel == Panel::Output,
    );
    draw_chain(frame, right[0], app);
    draw_params(frame, right[1], app);
    draw_meters(frame, root[2], app);
    draw_help(frame, root[3]);
}

fn draw_status(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let line = if let Some(session) = &app.session {
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
    frame.render_widget(
        Paragraph::new(line).block(
            Block::default()
                .borders(Borders::ALL)
                .title("crabjuice live"),
        ),
        area,
    );
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
        devices
            .iter()
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
        vec![ListItem::new("No slots. Press a to add Gain.")]
    } else {
        app.slots
            .iter()
            .enumerate()
            .map(|(index, slot)| {
                let selected = if index == app.selected_slot {
                    "> "
                } else {
                    "  "
                };
                let enabled = if slot.enabled { "on " } else { "off" };
                ListItem::new(format!(
                    "{selected}{index}: {:<7} {enabled}",
                    slot.kind.label()
                ))
            })
            .collect()
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
        lines.extend(param_lines(slot, app.selected_param));
        lines.extend([
            Line::from("Left/Right adjusts selected parameter."),
            Line::from("[/] selects parameter, t switches slot type."),
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

fn param_lines(slot: &ProcessorSlot, selected_param: usize) -> Vec<Line<'static>> {
    match slot.kind {
        ProcessorKind::Gain => vec![param_line(
            0,
            selected_param,
            format!("Gain: {:.2}", slot.gain),
        )],
        ProcessorKind::LowPass => vec![param_line(
            0,
            selected_param,
            format!("Cutoff: {:.0} Hz", slot.cutoff_hz),
        )],
        ProcessorKind::Delay => vec![
            param_line(0, selected_param, format!("Delay: {:.0} ms", slot.delay_ms)),
            param_line(1, selected_param, format!("Feedback: {:.2}", slot.feedback)),
            param_line(2, selected_param, format!("Mix: {:.2}", slot.mix)),
        ],
        ProcessorKind::Distortion => vec![
            param_line(0, selected_param, format!("Drive: {:.2}", slot.drive)),
            param_line(1, selected_param, format!("Mix: {:.2}", slot.mix)),
        ],
    }
}

fn param_line(index: usize, selected_param: usize, text: String) -> Line<'static> {
    if index == selected_param {
        Line::from(vec![
            Span::styled("> ", Style::default().fg(Color::Cyan)),
            Span::styled(text, Style::default().add_modifier(Modifier::BOLD)),
        ])
    } else {
        Line::from(format!("  {text}"))
    }
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
    let text = "Tab panels | Space start/stop | Enter apply device | r restart | a add | t type | d enable | x delete | [/] param | arrows navigate/adjust | q quit";
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
}
