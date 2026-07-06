use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use ratatui::Frame;

use crate::app::{App, Screen};

pub fn draw(frame: &mut Frame, app: &App) {
    let [body, status] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(frame.area());

    match app.screen {
        Screen::GameSelect => draw_game_select(frame, app, body),
        Screen::PlayerSetup => draw_player_setup(frame, app, body),
        Screen::Session => draw_session(frame, app, body),
    }

    if app.osk.is_some() {
        draw_osk(frame, app, body);
    }

    let status_line = Line::from(vec![
        Span::styled(" hyprcoop ", Style::new().fg(Color::Black).bg(Color::Cyan)),
        Span::raw(" "),
        Span::styled(&app.status, Style::new().fg(Color::Yellow)),
    ]);
    frame.render_widget(Paragraph::new(status_line), status);
}

fn draw_game_select(frame: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .handlers
        .iter()
        .enumerate()
        .map(|(i, loaded)| {
            let installed = if loaded.installed() {
                Span::styled("● installed", Style::new().fg(Color::Green))
            } else {
                Span::styled("○ not found", Style::new().fg(Color::Red))
            };
            let marker = if i == app.selected { "▶ " } else { "  " };
            let mut style = Style::new();
            if i == app.selected {
                style = style.add_modifier(Modifier::BOLD);
            }
            ListItem::new(Line::from(vec![
                Span::styled(format!("{marker}{}  ", loaded.handler.name), style),
                installed,
            ]))
        })
        .collect();

    let list = List::new(items).block(
        Block::new()
            .borders(Borders::ALL)
            .title(" Select game — ↑/↓ move · Enter continue · q quit "),
    );
    frame.render_widget(list, area);
}

fn draw_player_setup(frame: &mut Frame, app: &App, area: Rect) {
    let [slots_area, pads_area] =
        Layout::vertical([Constraint::Length(8), Constraint::Min(1)]).areas(area);

    let max = app.current_handler().handler.max_players as usize;
    let mut lines = Vec::new();
    for i in 0..max {
        let line = match app.slots.get(i) {
            Some(assignment) => Line::from(vec![
                Span::styled(
                    format!(" P{} ", i + 1),
                    Style::new().fg(Color::Black).bg(Color::Green),
                ),
                Span::raw(" "),
                Span::styled(assignment.label(), Style::new().fg(Color::Green)),
            ]),
            None => Line::from(vec![
                Span::styled(
                    format!(" P{} ", i + 1),
                    Style::new().fg(Color::Black).bg(Color::DarkGray),
                ),
                Span::styled(" — press a controller button to claim —", Style::new().fg(Color::DarkGray)),
            ]),
        };
        lines.push(line);
    }
    let slots = Paragraph::new(lines).block(Block::new().borders(Borders::ALL).title(format!(
        " Players — {} · m keyboard+mouse · Backspace remove · Enter launch · Esc back ",
        app.current_handler().handler.name
    )));
    frame.render_widget(slots, slots_area);

    let items: Vec<ListItem> = app
        .pads
        .iter()
        .map(|pad| {
            let claimed = app.pad_claimed(pad);
            let (marker, style) = match claimed {
                Some(slot) => (
                    format!("P{} ", slot + 1),
                    Style::new().fg(Color::Green),
                ),
                None => ("·  ".into(), Style::new()),
            };
            ListItem::new(Line::from(vec![
                Span::styled(marker, style),
                Span::raw(format!(
                    "{}  ({})",
                    pad.name,
                    pad.event_path.display()
                )),
            ]))
        })
        .collect();
    let pads = List::new(items).block(
        Block::new()
            .borders(Borders::ALL)
            .title(" Detected controllers (hotplug supported) "),
    );
    frame.render_widget(pads, pads_area);
}

fn draw_osk(frame: &mut Frame, app: &App, area: Rect) {
    let Some(osk) = &app.osk else { return };

    // Centered panel over the current screen.
    let width = 46.min(area.width.saturating_sub(2)).max(10);
    let height = 12.min(area.height.saturating_sub(2)).max(6);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    let panel = Rect::new(x, y, width, height);

    frame.render_widget(Clear, panel);

    let title = format!(
        " Controller Keyboard — P{}{} ",
        osk.player + 1,
        if osk.shifted() { " · ⇧" } else { "" }
    );
    let block = Block::new()
        .borders(Borders::ALL)
        .border_style(Style::new().fg(Color::Cyan))
        .title(title);
    let inner = block.inner(panel);
    frame.render_widget(block, panel);

    let [field_area, grid_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);

    let shown = osk.display().replace('\n', "⏎");
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(shown, Style::new().fg(Color::White)),
            Span::styled("_", Style::new().fg(Color::Cyan).add_modifier(Modifier::SLOW_BLINK)),
        ]))
        .block(Block::new().borders(Borders::BOTTOM)),
        field_area,
    );

    let lines: Vec<Line> = osk
        .grid()
        .into_iter()
        .map(|row| {
            let mut spans = Vec::new();
            for (label, selected) in row {
                let style = if selected {
                    Style::new()
                        .fg(Color::Black)
                        .bg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(Color::Gray)
                };
                spans.push(Span::styled(format!(" {label} "), style));
            }
            Line::from(spans)
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), grid_area);
}

fn draw_session(frame: &mut Frame, app: &App, area: Rect) {
    let mut lines = Vec::new();
    if let Some(session) = &app.session {
        for instance in &session.instances {
            let state = if instance.running() {
                Span::styled("running", Style::new().fg(Color::Green))
            } else {
                Span::styled("exited", Style::new().fg(Color::Red))
            };
            let window = match &instance.window {
                Some(_) => Span::styled(" · window adopted", Style::new().fg(Color::Cyan)),
                None => Span::styled(" · waiting for window…", Style::new().fg(Color::DarkGray)),
            };
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" P{} ", instance.player + 1),
                    Style::new().fg(Color::Black).bg(Color::Cyan),
                ),
                Span::raw(format!(" {}  ", instance.assignment.label())),
                state,
                window,
            ]));
        }
        lines.push(Line::raw(""));
        if let Some(notes) = &app.current_handler().handler.notes {
            for note_line in notes.lines().filter(|l| !l.trim().is_empty()) {
                lines.push(Line::styled(
                    format!(" {note_line}"),
                    Style::new().fg(Color::Yellow),
                ));
            }
            lines.push(Line::raw(""));
        }
        lines.push(Line::styled(
            " Games run on the 'coop' workspace. Switch back here to manage the session.",
            Style::new().fg(Color::DarkGray),
        ));
        lines.push(Line::styled(
            " Hold Options+Share / Menu+View / (+)&(−) for 2s to open the controller keyboard.",
            Style::new().fg(Color::DarkGray),
        ));
    }
    let panel = Paragraph::new(lines).block(
        Block::new()
            .borders(Borders::ALL)
            .title(" Session — e end session · q quit "),
    );
    frame.render_widget(panel, area);
}
