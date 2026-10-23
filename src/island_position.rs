//! Remembered on-screen position of the Dynamic Island, as layer-shell
//! margins from the top-left corner of its monitor (logical pixels).

use std::path::PathBuf;

pub const DEFAULT_TOP_MARGIN: i32 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IslandPos {
    pub x: i32,
    pub y: i32,
}

/// Keeps the whole island on the monitor; an island larger than the monitor
/// is pinned to the top-left corner.
pub fn clamp(pos: IslandPos, island: (i32, i32), monitor: (i32, i32)) -> IslandPos {
    IslandPos {
        x: pos.x.clamp(0, (monitor.0 - island.0).max(0)),
        y: pos.y.clamp(0, (monitor.1 - island.1).max(0)),
    }
}

pub fn centered(island_width: i32, monitor_width: i32) -> IslandPos {
    IslandPos {
        x: ((monitor_width - island_width) / 2).max(0),
        y: DEFAULT_TOP_MARGIN,
    }
}

fn parse(text: &str) -> Option<IslandPos> {
    let mut parts = text.split_whitespace();
    let x = parts.next()?.parse().ok()?;
    let y = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(IslandPos { x, y })
}

fn file() -> PathBuf {
    crate::bundled::config_dir().join("island-position")
}

pub fn load() -> Option<IslandPos> {
    parse(&std::fs::read_to_string(file()).ok()?)
}

pub fn save(pos: IslandPos) -> std::io::Result<()> {
    let path = file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, format!("{} {}\n", pos.x, pos.y))
}

pub fn forget() {
    let _ = std::fs::remove_file(file());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_saved_position() {
        assert_eq!(parse("120 48\n"), Some(IslandPos { x: 120, y: 48 }));
        assert_eq!(parse("-5 7"), Some(IslandPos { x: -5, y: 7 }));
        assert_eq!(parse(""), None);
        assert_eq!(parse("12"), None);
        assert_eq!(parse("1 2 3"), None);
        assert_eq!(parse("a b"), None);
    }

    #[test]
    fn clamps_to_monitor() {
        let island = (360, 40);
        let monitor = (1920, 1080);
        let inside = IslandPos { x: 100, y: 200 };
        assert_eq!(clamp(inside, island, monitor), inside);
        assert_eq!(
            clamp(IslandPos { x: -40, y: -3 }, island, monitor),
            IslandPos { x: 0, y: 0 }
        );
        assert_eq!(
            clamp(IslandPos { x: 5000, y: 5000 }, island, monitor),
            IslandPos { x: 1560, y: 1040 }
        );
        assert_eq!(
            clamp(IslandPos { x: 50, y: 50 }, (2000, 1200), monitor),
            IslandPos { x: 0, y: 0 }
        );
    }

    #[test]
    fn centers_horizontally_below_top_edge() {
        assert_eq!(centered(360, 1920), IslandPos { x: 780, y: DEFAULT_TOP_MARGIN });
        assert_eq!(centered(2000, 1920).x, 0);
    }
}
