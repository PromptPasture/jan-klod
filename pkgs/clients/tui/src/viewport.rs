//! Which transcript lines are on screen.

/// A window over rendered lines: where it starts, and whether it is following the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    offset: usize,
    attached: bool,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            offset: 0,
            attached: true,
        }
    }
}

impl Viewport {
    const fn max_offset(total: usize, height: usize) -> usize {
        total.saturating_sub(height)
    }

    /// Index of the first visible line.
    #[must_use]
    pub const fn offset(self) -> usize {
        self.offset
    }

    /// Whether the view is following the bottom.
    #[must_use]
    pub const fn attached(self) -> bool {
        self.attached
    }

    /// Lines below the window.
    #[must_use]
    pub const fn below(self, total: usize, height: usize) -> usize {
        Self::max_offset(total, height).saturating_sub(self.offset)
    }

    /// Recalculates after a size change.
    #[must_use]
    pub const fn reflow(mut self, total: usize, height: usize) -> Self {
        let max = Self::max_offset(total, height);
        if self.attached {
            self.offset = max;
        } else if self.offset > max {
            self.offset = max;
            self.attached = true;
        }
        self
    }

    /// Scrolls the least that brings `[start, start + rows)` on screen.
    #[must_use]
    pub const fn reveal(mut self, start: usize, rows: usize, total: usize, height: usize) -> Self {
        let max = Self::max_offset(total, height);
        let end = start + rows;
        if start < self.offset {
            self.offset = start;
        } else if end > self.offset + height {
            let wanted = end.saturating_sub(height);
            self.offset = if wanted > max { max } else { wanted };
        }
        self.attached = self.offset >= max;
        self
    }

    /// Scroll up by `rows`, detaching if it moves.
    #[must_use]
    pub const fn up(mut self, rows: usize) -> Self {
        let moved = self.offset.saturating_sub(rows);
        if moved != self.offset {
            self.offset = moved;
            self.attached = false;
        }
        self
    }

    /// Scroll down by `rows`, re-attaching on arrival at the bottom.
    #[must_use]
    pub const fn down(mut self, rows: usize, total: usize, height: usize) -> Self {
        let max = Self::max_offset(total, height);
        let moved = self.offset + rows;
        self.offset = if moved > max { max } else { moved };
        if self.offset == max {
            self.attached = true;
        }
        self
    }

    /// Jump to the bottom and re-attach.
    #[must_use]
    pub const fn to_bottom(self, total: usize, height: usize) -> Self {
        Self {
            offset: Self::max_offset(total, height),
            attached: true,
        }
    }

    /// A full page up.
    #[must_use]
    pub const fn page_up(self, height: usize) -> Self {
        self.up(height)
    }

    /// A full page down.
    #[must_use]
    pub const fn page_down(self, total: usize, height: usize) -> Self {
        self.down(height, total, height)
    }

    /// Half a page up.
    #[must_use]
    pub const fn half_up(self, height: usize) -> Self {
        self.up(height / 2)
    }

    /// Half a page down.
    #[must_use]
    pub const fn half_down(self, total: usize, height: usize) -> Self {
        self.down(height / 2, total, height)
    }
}

#[cfg(test)]
mod tests {
    use super::Viewport;

    const H: usize = 10;

    #[test]
    fn a_transcript_taller_than_the_pane_shows_its_bottom() {
        let v = Viewport::default().reflow(100, H);
        assert_eq!(v.offset(), 90, "the last ten of a hundred lines");
        assert!(v.attached());
        assert_eq!(v.below(100, H), 0, "nothing is below an attached view");
    }

    #[test]
    fn a_transcript_shorter_than_the_pane_stays_at_the_top() {
        let v = Viewport::default().reflow(3, H);
        assert_eq!(v.offset(), 0);
        assert!(v.attached(), "there is no 'up' to have scrolled to");
        assert_eq!(v.below(3, H), 0);
    }

    #[test]
    fn scrolling_up_detaches_and_the_top_is_reachable() {
        let mut v = Viewport::default().reflow(100, H);
        for _ in 0..20 {
            v = v.page_up(H);
        }
        assert_eq!(v.offset(), 0, "the top is reachable");
        assert!(!v.attached(), "scrolling up detaches");
        assert_eq!(v.below(100, H), 90, "the marker has a count to show");
    }

    #[test]
    fn a_delta_arriving_while_detached_does_not_move_the_view() {
        let detached = Viewport::default().reflow(100, H).page_up(H);
        assert!(!detached.attached());
        let before = detached.offset();

        let mut v = detached;
        for extra in 1..=25 {
            v = v.reflow(100 + extra, H);
            assert_eq!(
                v.offset(),
                before,
                "line {extra} moved a detached view — this is the yank"
            );
            assert!(!v.attached());
        }
        assert_eq!(v.below(125, H), 125 - H - before);
    }

    #[test]
    fn a_delta_arriving_while_attached_follows_the_bottom() {
        let mut v = Viewport::default().reflow(100, H);
        for extra in 1..=25 {
            v = v.reflow(100 + extra, H);
            assert_eq!(v.offset(), 100 + extra - H, "an attached view lagged");
            assert!(v.attached());
        }
    }

    #[test]
    fn returning_to_the_bottom_reattaches_by_either_route() {
        let detached = Viewport::default().reflow(100, H).page_up(H);
        assert!(!detached.attached());

        assert!(detached.to_bottom(100, H).attached());

        let mut v = detached;
        for _ in 0..10 {
            v = v.page_down(100, H);
        }
        assert_eq!(v.offset(), 90);
        assert!(v.attached(), "scrolling back to the end re-attached");
    }

    #[test]
    fn a_pane_that_grows_past_the_transcript_does_not_leave_blank_rows() {
        let detached = Viewport::default().reflow(100, H).page_up(H);
        let v = detached.reflow(100, 200);
        assert_eq!(v.offset(), 0);
        assert!(
            v.attached(),
            "clamped to bottom re-attaches; avoids state divergence on next append"
        );
    }

    #[test]
    fn half_pages_move_half_as_far() {
        let bottom = Viewport::default().reflow(100, H);
        assert_eq!(bottom.page_up(H).offset(), 80);
        assert_eq!(bottom.half_up(H).offset(), 85);
        let up = bottom.page_up(H).page_up(H);
        assert_eq!(up.half_down(100, H).offset(), 75);
    }

    #[test]
    fn reveal_scrolls_the_least_that_brings_a_block_on_screen() {
        let bottom = Viewport::default().reflow(100, 10);
        assert_eq!(bottom.offset(), 90);

        assert_eq!(bottom.reveal(92, 3, 100, 10), bottom);

        let up = bottom.reveal(20, 3, 100, 10);
        assert_eq!(up.offset(), 20);
        assert!(!up.attached(), "scrolling back detaches");

        let down = up.reveal(40, 2, 100, 10);
        assert_eq!(down.offset(), 32, "the block's last line is the last row");
        assert!(!down.attached());

        let back = up.reveal(97, 3, 100, 10);
        assert_eq!(back.offset(), 90);
        assert!(back.attached(), "arriving at the bottom re-attaches");
    }

    #[test]
    fn a_block_taller_than_the_pane_is_shown_from_its_top() {
        let view = Viewport::default().reflow(100, 10).reveal(30, 40, 100, 10);
        assert_eq!(view.offset(), 30, "its head row is where the name is");
    }
}
