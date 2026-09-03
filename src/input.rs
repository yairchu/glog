use crate::app::{App, Mode};
use crossterm::event::{Event, KeyCode, KeyEventKind, MouseButton, MouseEventKind};

const LOG_TAB_START: u16 = 7;
const LOG_TAB_END: u16 = 12;
const SHOW_TAB_START: u16 = 13;
const SHOW_TAB_END: u16 = 19;

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
        if app.show_help {
            match key.code {
                KeyCode::Esc | KeyCode::Char('h') | KeyCode::Char('q') => app.show_help = false,
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char('q') => app.quit = true,
            KeyCode::Tab => app.switch_mode(),
            KeyCode::Enter if app.mode == Mode::Log => app.switch_mode(),
            KeyCode::Esc if app.mode == Mode::Show => app.switch_mode(),
            KeyCode::Up | KeyCode::Char('k') => app.move_by(-1, 1),
            KeyCode::Down | KeyCode::Char('j') => app.move_by(1, 1),
            KeyCode::PageUp | KeyCode::Char('b') => app.move_by(-2, 20),
            KeyCode::PageDown | KeyCode::Char(' ') => app.move_by(2, 20),
            KeyCode::Char('f') if app.mode == Mode::Show => app.move_by(2, 20),
            KeyCode::Left if app.mode == Mode::Show => show_adjacent(app, -1),
            KeyCode::Right if app.mode == Mode::Show => show_adjacent(app, 1),
            KeyCode::Char('g') | KeyCode::Home => app.top(),
            KeyCode::Char('G') | KeyCode::End => app.bottom(),
            KeyCode::Char('/') => app.begin_search(false),
            KeyCode::Char('?') => app.begin_search(true),
            KeyCode::Char('n') => app.repeat_search(false),
            KeyCode::Char('N') => app.repeat_search(true),
            KeyCode::Char('h') => app.show_help = true,
            _ => {}
        }
    } else if let Event::Mouse(mouse) = event {
        match mouse.kind {
            MouseEventKind::ScrollUp => app.move_by(-1, 3),
            MouseEventKind::ScrollDown => app.move_by(1, 3),
            MouseEventKind::Down(MouseButton::Left) if mouse.row == 0 => {
                if (LOG_TAB_START..LOG_TAB_END).contains(&mouse.column) {
                    app.mode = Mode::Log;
                    app.show_help = false;
                } else if (SHOW_TAB_START..SHOW_TAB_END).contains(&mouse.column) {
                    if app.mode != Mode::Show {
                        app.switch_mode();
                    }
                    app.show_help = false;
                }
            }
            MouseEventKind::Down(MouseButton::Left)
                if app.mode == Mode::Log && mouse.row >= app.log_row_origin =>
            {
                let row = usize::from(mouse.row - app.log_row_origin);
                if let Some(Some(selected)) = app.visible_log_rows.get(row).copied() {
                    if app.selected == selected {
                        app.switch_mode();
                    } else {
                        app.selected = selected;
                    }
                }
            }
            _ => {}
        }
    }
}

fn show_adjacent(app: &mut App, delta: isize) {
    if app.move_selection(delta) {
        app.show_offset = 0;
        app.load_show();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers, MouseEvent};

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn enter_opens_show_and_escape_returns_to_log() {
        let mut app = App::new(Vec::new());
        handle(key(KeyCode::Enter), &mut app);
        assert_eq!(app.mode, Mode::Show);
        handle(key(KeyCode::Esc), &mut app);
        assert_eq!(app.mode, Mode::Log);
        handle(key(KeyCode::Esc), &mut app);
        assert!(!app.quit);
    }

    #[test]
    fn question_mark_starts_reverse_search() {
        let mut app = App::new(Vec::new());
        handle(key(KeyCode::Char('?')), &mut app);
        assert_eq!(app.search_input.as_deref(), Some(""));
        assert!(app.search_reverse);
    }

    #[test]
    fn help_captures_escape() {
        let mut app = App::new(Vec::new());
        handle(key(KeyCode::Char('h')), &mut app);
        assert!(app.show_help);
        handle(key(KeyCode::Esc), &mut app);
        assert!(!app.show_help);
    }

    #[test]
    fn header_tabs_are_clickable() {
        let mut app = App::new(Vec::new());
        let click = |column| {
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row: 0,
                modifiers: KeyModifiers::NONE,
            })
        };
        handle(click(SHOW_TAB_START), &mut app);
        assert_eq!(app.mode, Mode::Show);
        handle(click(LOG_TAB_START), &mut app);
        assert_eq!(app.mode, Mode::Log);
    }

    #[test]
    fn clicking_a_log_row_selects_then_opens_its_commit() {
        let mut app = App::new(Vec::new());
        app.log_row_origin = 1;
        app.visible_log_rows = vec![Some(4), Some(5), Some(6)];
        handle(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 20,
                row: 2,
                modifiers: KeyModifiers::NONE,
            }),
            &mut app,
        );
        assert_eq!(app.selected, 5);
        assert_eq!(app.mode, Mode::Log);
        handle(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 20,
                row: 2,
                modifiers: KeyModifiers::NONE,
            }),
            &mut app,
        );
        assert_eq!(app.mode, Mode::Show);
    }

    #[test]
    fn f_pages_forward_in_show() {
        let mut app = App::new(Vec::new());
        app.mode = Mode::Show;
        handle(key(KeyCode::Char('f')), &mut app);
        assert_eq!(app.show_offset, 20);
    }
}
