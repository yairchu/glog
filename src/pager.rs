//! Full-screen text views paged with the shared pager keys.
use ratatui::{layout::Rect, Frame};

/// A view that `App` pages, searches, and draws without knowing its kind.
pub trait PagerView {
    /// The line that search matches and the help mark refer to.
    fn cursor(&self) -> usize;
    fn move_by(&mut self, delta: isize);
    /// Move one screen in the direction of `delta`.
    fn page(&mut self, delta: isize);
    /// Follow the mouse wheel.
    fn wheel(&mut self, delta: isize);
    fn top(&mut self);
    fn bottom(&mut self);
    /// Move the cursor to the next line containing the lowercase `query`.
    fn find(&mut self, query: &str, reverse: bool) -> bool;
    fn draw(&mut self, frame: &mut Frame, area: Rect, query: Option<&str>, current: Option<usize>);
}
