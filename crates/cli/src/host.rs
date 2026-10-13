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

/// Current host size as `(cols, rows)`. A PTY whose window size was never set reports 0; such a
/// dimension falls back to the classic 80×24 so attach still works (the daemon rejects 0).
pub fn size() -> io::Result<(u16, u16)> {
    let (cols, rows) = crossterm::terminal::size()?;
    Ok(nonzero_size(cols, rows))
}

fn nonzero_size(cols: u16, rows: u16) -> (u16, u16) {
    (
        if cols == 0 { 80 } else { cols },
        if rows == 0 { 24 } else { rows },
    )
}

#[cfg(test)]
mod tests {
    use super::nonzero_size;

    #[test]
    fn unset_dimensions_fall_back_independently() {
        assert_eq!(nonzero_size(0, 0), (80, 24));
        assert_eq!(nonzero_size(0, 40), (80, 40));
        assert_eq!(nonzero_size(132, 0), (132, 24));
        assert_eq!(nonzero_size(1, 1), (1, 1));
    }
}
