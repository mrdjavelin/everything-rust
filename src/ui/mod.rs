mod components;
mod layout;
mod render;
mod theme;

#[cfg(test)]
mod tests;

use std::{io, time::Duration};

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::app::App;

pub struct TerminalUi {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    restored: bool,
}

impl TerminalUi {
    pub fn new() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(error);
        }

        match Terminal::new(CrosstermBackend::new(stdout)) {
            Ok(mut terminal) => {
                if let Err(error) = terminal.hide_cursor() {
                    let _ = execute!(terminal.backend_mut(), LeaveAlternateScreen);
                    let _ = disable_raw_mode();
                    return Err(error);
                }
                Ok(Self {
                    terminal,
                    restored: false,
                })
            }
            Err(error) => {
                let _ = execute!(io::stdout(), LeaveAlternateScreen);
                let _ = disable_raw_mode();
                Err(error)
            }
        }
    }

    pub fn draw(&mut self, app: &App) -> io::Result<()> {
        self.terminal.draw(|frame| render::draw(frame, app))?;
        Ok(())
    }

    pub fn poll_event(&self, timeout: Duration) -> io::Result<Option<Event>> {
        if event::poll(timeout)? {
            event::read().map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn restore(&mut self) -> io::Result<()> {
        let cursor = self.terminal.show_cursor();
        let screen = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let raw_mode = disable_raw_mode();
        self.restored = true;
        cursor.and(screen).and(raw_mode)
    }
}

impl Drop for TerminalUi {
    fn drop(&mut self) {
        if self.restored {
            return;
        }
        let _ = self.terminal.show_cursor();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}
