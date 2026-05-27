use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::agent::agent_loop::StreamEvent;

/// Manages the terminal UI, rendering output and handling input.
pub struct Tui {
    output_lines: Vec<(String, Color)>,
    input_buffer: String,
    scroll_offset: usize,
    model: String,
    token_count: String,
    session_status: String,
    rx_events: mpsc::UnboundedReceiver<StreamEvent>,
    tx_input: mpsc::UnboundedSender<String>,
}

impl Tui {
    pub fn new(
        rx_events: mpsc::UnboundedReceiver<StreamEvent>,
        tx_input: mpsc::UnboundedSender<String>,
    ) -> Self {
        Self {
            output_lines: Vec::new(),
            input_buffer: String::new(),
            scroll_offset: 0,
            model: "deepseek-v4-flash".into(),
            token_count: "0".into(),
            session_status: "Ready".into(),
            rx_events,
            tx_input,
        }
    }

    /// Run the TUI event loop. Blocks until user quits.
    pub fn run(&mut self) -> io::Result<()> {
        // Enter raw mode + alternate screen
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        stdout.execute(EnterAlternateScreen)?;

        // Restore terminal on panic
        let orig_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = terminal::disable_raw_mode();
            let _ = io::stdout().execute(LeaveAlternateScreen);
            orig_hook(info);
        }));

        info!("tui: started");

        let backend = ratatui::backend::CrosstermBackend::new(stdout);
        let mut terminal = ratatui::Terminal::new(backend)?;

        loop {
            // Process incoming events from agent
            while let Ok(event) = self.rx_events.try_recv() {
                self.handle_stream_event(event);
            }

            // Draw
            terminal.draw(|f| self.render(f))?;

            // Check for input (non-blocking)
            if event::poll(std::time::Duration::from_millis(50))? {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        if !self.handle_key(key) {
                            break; // quit
                        }
                    }
                    Event::Resize(_, _) => {
                        debug!("tui: resize event");
                    }
                    _ => {}
                }
            }
        }

        // Restore terminal
        terminal::disable_raw_mode()?;
        let mut stdout = io::stdout();
        stdout.execute(LeaveAlternateScreen)?;

        info!("tui: stopped");
        Ok(())
    }

    fn handle_stream_event(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::Text { text, .. } => {
                // Append to last output line if it's assistant text, otherwise new line
                if let Some((last, _)) = self.output_lines.last_mut() {
                    last.push_str(&text);
                } else {
                    self.output_lines.push((text, Color::White));
                }
            }
            StreamEvent::ToolCallStart { tool, args, .. } => {
                self.output_lines
                    .push((format!("⚙ {tool} {args}"), Color::Yellow));
            }
            StreamEvent::ToolCallEnd { tool, output, is_error, .. } => {
                let color = if is_error { Color::Red } else { Color::Green };
                let preview: String = output.lines().take(3).collect::<Vec<_>>().join("\n");
                self.output_lines.push((format!("  → {tool}: {preview}"), color));
            }
            StreamEvent::TurnEnd { finish_reason, .. } => {
                self.output_lines
                    .push((format!("--- turn end ({finish_reason}) ---"), Color::DarkGray));
            }
            StreamEvent::SessionReset => {
                self.output_lines.clear();
                self.output_lines
                    .push(("Session reset".into(), Color::Cyan));
                self.session_status = "Reset".into();
            }
            StreamEvent::Error { message } => {
                self.output_lines
                    .push((format!("ERROR: {message}"), Color::Red));
            }
        }
    }

    fn handle_key(&mut self, key: event::KeyEvent) -> bool {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => {
                return false; // quit
            }
            KeyCode::Enter => {
                let input = std::mem::take(&mut self.input_buffer);
                if !input.trim().is_empty() {
                    self.output_lines.push((format!("> {input}"), Color::Blue));
                    let _ = self.tx_input.send(input);
                }
            }
            KeyCode::Char(c) => {
                self.input_buffer.push(c);
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
            }
            KeyCode::Up => {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
            }
            KeyCode::Down => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }
            KeyCode::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_add(10);
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_sub(10);
            }
            KeyCode::Tab => {
                // Simple autocomplete: cycle common commands
                let suggestions = vec!["/help", "/clear", "/quit", "/reset"];
                for s in suggestions {
                    if s.starts_with(&self.input_buffer) && s != self.input_buffer {
                        self.input_buffer = s.to_string();
                        break;
                    }
                }
            }
            _ => {}
        }
        true
    }

    fn render(&self, f: &mut Frame) {
        let size = f.area();

        // Layout: output (flex) | input (3 lines) | status (1 line)
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),      // output
                Constraint::Length(3),   // input
                Constraint::Length(1),   // status
            ])
            .split(size);

        self.render_output(f, chunks[0]);
        self.render_input(f, chunks[1]);
        self.render_status(f, chunks[2]);
    }

    fn render_output(&self, f: &mut Frame, area: Rect) {
        let visible_height = area.height.saturating_sub(2) as usize; // account for borders
        let total_lines = self.output_lines.len();

        let start = if total_lines > visible_height {
            let max_scroll = total_lines.saturating_sub(visible_height);
            let offset = self.scroll_offset.min(max_scroll);
            total_lines.saturating_sub(visible_height + offset)
        } else {
            0
        };
        let end = (start + visible_height).min(total_lines);

        let lines: Vec<Line> = self.output_lines[start..end]
            .iter()
            .map(|(text, color)| {
                Line::from(Span::styled(
                    text.clone(),
                    Style::default().fg(*color),
                ))
            })
            .collect();

        let paragraph = Paragraph::new(Text::from(lines))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Output "),
            )
            .wrap(Wrap { trim: false });

        f.render_widget(paragraph, area);
    }

    fn render_input(&self, f: &mut Frame, area: Rect) {
        let cursor_pos = self.input_buffer.len();
        let display_text = if self.input_buffer.is_empty() {
            "Type your message... (Ctrl+C to quit)".to_string()
        } else {
            self.input_buffer.clone()
        };

        let paragraph = Paragraph::new(display_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Input "),
            )
            .style(Style::default().fg(Color::White));

        f.render_widget(paragraph, area);

        // Set cursor position
        if !self.input_buffer.is_empty() {
            f.set_cursor_position((
                area.x + 1 + cursor_pos as u16,
                area.y + 1,
            ));
        }
    }

    fn render_status(&self, f: &mut Frame, area: Rect) {
        let status = Line::from(vec![
            Span::styled(
                format!(" Model: {} ", self.model),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("| Tokens: {} ", self.token_count),
                Style::default().fg(Color::Gray),
            ),
            Span::styled(
                format!("| {}", self.session_status),
                Style::default().fg(Color::Green),
            ),
            Span::styled(
                " | Ctrl+C quit, ↑↓ scroll",
                Style::default().fg(Color::DarkGray),
            ),
        ]);

        let paragraph = Paragraph::new(status).block(Block::default());
        f.render_widget(paragraph, area);
    }
}
