use crate::app::App;
use crossterm::event::{Event, KeyCode, KeyEventKind, MouseEventKind};

pub fn handle(event: Event, app: &mut App) {
    if let Event::Key(key) = event {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if app.search_input.is_some() {
            match key.code {
                KeyCode::Esc => app.search_input = None,
                KeyCode::Enter => app.submit_search(),
                KeyCode::Backspace => {
                    app.search_input.as_mut().unwrap().pop();
                }
                KeyCode::Char(c) => app.search_input.as_mut().unwrap().push(c),
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char('q') => app.quit = true,
            KeyCode::Tab => app.switch_mode(),
            KeyCode::Up | KeyCode::Char('k') => app.move_by(-1, 1),
            KeyCode::Down | KeyCode::Char('j') => app.move_by(1, 1),
            KeyCode::PageUp | KeyCode::Char('b') => app.move_by(-2, 20),
            KeyCode::PageDown | KeyCode::Char(' ') => app.move_by(2, 20),
            KeyCode::Char('g') | KeyCode::Home => app.top(),
            KeyCode::Char('G') | KeyCode::End => app.bottom(),
            KeyCode::Char('/') => app.begin_search(),
            KeyCode::Char('n') => app.next_match(false),
            KeyCode::Char('N') => app.next_match(true),
            _ => {}
        }
    } else if let Event::Mouse(mouse) = event {
        match mouse.kind {
            MouseEventKind::ScrollUp => app.move_by(-1, 3),
            MouseEventKind::ScrollDown => app.move_by(1, 3),
            _ => {}
        }
    }
}
