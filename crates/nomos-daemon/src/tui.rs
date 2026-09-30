use crate::client::NomosClient;
use crate::ipc::default_socket_path;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Gauge, Paragraph, Row, Table},
    Terminal,
};
use std::io;
use std::path::PathBuf;
use std::time::Duration;

pub async fn run_tui(socket_path: Option<PathBuf>) -> Result<(), anyhow::Error> {
    let sock = socket_path.unwrap_or_else(default_socket_path);
    let mut client = NomosClient::connect(&sock).await
        .map_err(|e| anyhow::anyhow!("Cannot connect to Nomos arbiter at {:?}: {}. Is 'nomos daemon' running?", sock, e))?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut should_quit = false;

    while !should_quit {
        let status = client.get_status().await?;

        terminal.draw(|f| {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .margin(1)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Min(6),
                    Constraint::Length(1),
                ])
                .split(f.area());

            // Header
            let header = Paragraph::new(format!(
                " NOMOS RESOURCE ARBITER (Press 'q' to exit) - Active: {} | Queued: {}",
                status.active_leases.len(),
                status.queued_leases.len()
            ))
            .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL).title("Nomos Status"));
            f.render_widget(header, chunks[0]);

            // CPU Gauge
            let cpu_total = status.pool.total_cores;
            let cpu_used = status.allocated_cpu;
            let cpu_pct = if cpu_total > 0.0 { ((cpu_used / cpu_total) * 100.0) as u16 } else { 0 };
            let cpu_gauge = Gauge::default()
                .block(Block::default().borders(Borders::ALL).title("CPU Budget Allocation"))
                .gauge_style(Style::default().fg(Color::Blue))
                .percent(cpu_pct.min(100))
                .label(format!("{:.1} / {:.1} Cores ({}%)", cpu_used, cpu_total, cpu_pct));
            f.render_widget(cpu_gauge, chunks[1]);

            // RAM Gauge
            let mem_total_gb = status.pool.total_memory_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
            let mem_used_gb = status.allocated_memory_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
            let mem_pct = if mem_total_gb > 0.0 { ((mem_used_gb / mem_total_gb) * 100.0) as u16 } else { 0 };
            let mem_gauge = Gauge::default()
                .block(Block::default().borders(Borders::ALL).title("RAM Budget Allocation"))
                .gauge_style(Style::default().fg(Color::Green))
                .percent(mem_pct.min(100))
                .label(format!("{:.2} / {:.2} GiB ({}%)", mem_used_gb, mem_total_gb, mem_pct));
            f.render_widget(mem_gauge, chunks[2]);

            // Table of active leases
            let rows: Vec<Row> = status.active_leases.iter().map(|l| {
                let mem_gb = l.request.req_memory_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
                Row::new(vec![
                    l.id.clone(),
                    l.request.worker_id.clone(),
                    format!("{:.1}", l.request.req_cpu),
                    format!("{:.2} GiB", mem_gb),
                    format!("{:?}", l.request.priority),
                    format!("{:?}", l.state),
                ])
            }).collect();

            let table = Table::new(
                rows,
                [
                    Constraint::Length(14),
                    Constraint::Percentage(30),
                    Constraint::Length(10),
                    Constraint::Length(14),
                    Constraint::Length(12),
                    Constraint::Length(12),
                ],
            )
            .header(
                Row::new(vec!["LEASE ID", "WORKER", "VCPU", "RAM", "PRIORITY", "STATE"])
                    .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            )
            .block(Block::default().borders(Borders::ALL).title("Active Worker Leases"));

            f.render_widget(table, chunks[3]);

            // Footer
            let footer = Paragraph::new("Press 'q' to quit | Realtime socket telemetry @ 60 FPS")
                .style(Style::default().fg(Color::DarkGray));
            f.render_widget(footer, chunks[4]);
        })?;

        if event::poll(Duration::from_millis(500))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') {
                    should_quit = true;
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}
