//! The frame's regions (#97, #104), as a pure function of the terminal size.
//!
//! [`regions`] takes nothing but `(width, height)` and returns which of the
//! header, sidebar and status bar exist and where — so #104's Acceptance can
//! test the responsive table (120×32, 90×30, 70×24, 50×18, 60×8) without a
//! terminal at all. The transcript/composer split inside [`Frame::content`]
//! stays where it was: that boundary depends on the composer's *content*
//! (#148 already made it grow with the buffer), which this function has no way
//! to know and does not try to.

use ratatui::layout::Rect;

/// The sidebar's fixed width, exactly as #104's Scope states it.
const SIDEBAR_WIDTH: u16 = 28;
/// Below this many rows for the transcript+composer area, there is nothing a
/// normal frame can show — #104's "the transcript never goes below 5 rows".
const MIN_TRANSCRIPT_HEIGHT: u16 = 5;
/// The smallest a bordered composer can be: one row of text, two of border.
const MIN_COMPOSER_HEIGHT: u16 = 3;

/// Frame regions for one `(width, height)` (#104's responsive table).
/// `header` and `sidebar` are `None` where hidden. `content` is transcript+composer
/// (split later depends on composer buffer, not terminal size).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// The one-row header, when the terminal is tall enough for it.
    pub header: Option<Rect>,
    /// The 28-column sidebar, when the terminal is both wide and tall enough.
    pub sidebar: Option<Rect>,
    /// The transcript+composer area — always present, even when
    /// [`Frame::too_small`] means it holds a "terminal too small" panel
    /// instead of the normal panes.
    pub content: Rect,
    /// The one-row status bar.
    pub status: Rect,
    /// `content` cannot fit a transcript and a composer at their smallest —
    /// draw the "terminal too small" panel there instead of the normal frame.
    pub too_small: bool,
    /// 60–79 columns: the header (when shown) collapses to session id + turn
    /// state, and the status bar shows fewer hints.
    pub collapsed_header: bool,
    /// Below 60 columns: single column, the transcript loses its border, and
    /// the composer stays one row until it is focused.
    pub single_column: bool,
}

/// The frame's regions for `width` × `height`, implementing #97's responsive
/// table exactly as #104 quotes it:
///
/// | Width | Behaviour |
/// |---|---|
/// | ≥ 100 | as drawn |
/// | 80–99 | sidebar hidden |
/// | 60–79 | header collapses; fewer status hints |
/// | < 60 | single column, transcript loses its border, composer 1 row until focused |
///
/// Height: below 20 rows drop the sidebar, then the header, before the
/// transcript loses height.
#[must_use]
pub fn regions(width: u16, height: u16) -> Frame {
    let header_visible = height >= 20;
    let sidebar_visible = header_visible && width >= 100;
    let collapsed_header = header_visible && width < 80;
    let single_column = width < 60;

    let mut y = 0u16;
    let header = if header_visible {
        let rect = Rect {
            x: 0,
            y,
            width,
            height: 1,
        };
        y += 1;
        Some(rect)
    } else {
        None
    };

    let status_y = height.saturating_sub(1).max(y);
    let status = Rect {
        x: 0,
        y: status_y,
        width,
        height: height.saturating_sub(status_y).min(1),
    };

    let content_height = status_y.saturating_sub(y);
    let content_width = if sidebar_visible {
        width.saturating_sub(SIDEBAR_WIDTH)
    } else {
        width
    };
    let content = Rect {
        x: 0,
        y,
        width: content_width,
        height: content_height,
    };
    let sidebar = sidebar_visible.then_some(Rect {
        x: content_width,
        y,
        width: SIDEBAR_WIDTH,
        height: content_height,
    });

    let too_small = content_height < MIN_TRANSCRIPT_HEIGHT + MIN_COMPOSER_HEIGHT;

    Frame {
        header,
        sidebar,
        content,
        status,
        too_small,
        collapsed_header,
        single_column,
    }
}

#[cfg(test)]
mod tests {
    use super::{regions, Frame};

    /// Whether two rects share a cell.
    fn overlaps(a: ratatui::layout::Rect, b: ratatui::layout::Rect) -> bool {
        a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
    }

    /// No overlaps, no regions exceed terminal bounds ("clipped mid-glyph" starts with bad rect).
    fn assert_tiles_cleanly(frame: Frame, width: u16, height: u16) {
        let mut present: Vec<ratatui::layout::Rect> = vec![frame.content, frame.status];
        present.extend(frame.header);
        present.extend(frame.sidebar);

        for rect in &present {
            assert!(
                rect.x + rect.width <= width && rect.y + rect.height <= height,
                "{rect:?} does not fit in {width}x{height}"
            );
        }
        for (i, a) in present.iter().enumerate() {
            for b in &present[i + 1..] {
                assert!(!overlaps(*a, *b), "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn one_hundred_twenty_by_thirty_two_is_drawn_in_full() {
        let frame = regions(120, 32);
        assert!(frame.header.is_some(), "the header is hidden");
        assert!(frame.sidebar.is_some(), "the sidebar is hidden");
        assert!(!frame.too_small);
        assert!(!frame.collapsed_header);
        assert!(!frame.single_column);
        assert_eq!(frame.sidebar.unwrap().width, 28);
        assert_tiles_cleanly(frame, 120, 32);
    }

    #[test]
    fn ninety_by_thirty_hides_only_the_sidebar() {
        let frame = regions(90, 30);
        assert!(frame.header.is_some(), "80-99 still shows the header");
        assert!(frame.sidebar.is_none(), "80-99 must hide the sidebar");
        assert!(
            !frame.collapsed_header,
            "80-99 is not the 60-79 collapse band"
        );
        assert!(!frame.too_small);
        assert_tiles_cleanly(frame, 90, 30);
    }

    #[test]
    fn seventy_by_twenty_four_collapses_the_header() {
        let frame = regions(70, 24);
        assert!(frame.header.is_some(), "the header still exists, collapsed");
        assert!(frame.sidebar.is_none());
        assert!(frame.collapsed_header, "60-79 must collapse the header");
        assert!(!frame.single_column, "70 columns is not below 60");
        assert!(!frame.too_small);
        assert_tiles_cleanly(frame, 70, 24);
    }

    #[test]
    fn fifty_by_eighteen_drops_the_header_too() {
        let frame = regions(50, 18);
        assert!(
            frame.header.is_none(),
            "below 20 rows drops the header, after the sidebar"
        );
        assert!(frame.sidebar.is_none());
        assert!(frame.single_column, "50 columns is below 60");
        assert!(!frame.too_small, "18 rows still fits a minimal frame");
        assert_tiles_cleanly(frame, 50, 18);
    }

    /// Acceptance: a render at 60×8 is the "terminal too small" panel.
    #[test]
    fn sixty_by_eight_is_too_small() {
        let frame = regions(60, 8);
        assert!(
            frame.too_small,
            "8 rows cannot hold a transcript and a composer"
        );
        assert_tiles_cleanly(frame, 60, 8);
    }

    /// Above tests check the acceptance's four points; this sweeps wider to ensure invariant holds.
    #[test]
    fn regions_never_overlap_or_run_past_the_terminal_at_any_size() {
        for width in [10, 30, 59, 60, 79, 80, 99, 100, 200] {
            for height in [3, 8, 15, 19, 20, 21, 50] {
                assert_tiles_cleanly(regions(width, height), width, height);
            }
        }
    }
}
