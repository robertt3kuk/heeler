//! Host terminal access for `attach`: raw mode and size.

use std::io;

/// Puts the host terminal in raw mode; restores the previous mode on drop.
#[derive(Debug)]
pub struct RawMode(());

impl RawMode {
    pub fn enable() -> io::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self(()))
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // Nothing useful to do if the terminal is gone.
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Current host size as `(cols, rows)`.
pub fn size() -> io::Result<(u16, u16)> {
    crossterm::terminal::size()
}
