//! Which of the transcript's lines are on screen (#148).
//!
//! The current client always draws from the first line, so a transcript longer
//! than the pane is a transcript whose end you cannot see. This is the model
//! that fixes that — and the reason it is a model rather than a widget is the
//! requirement underneath the whole slice: **a `text-delta` arriving while the
//! user has scrolled up must not move the view.** That is a statement about
//! state over time, so it is tested as one, without a terminal.
//!
//! # Attached, and what detaches it
//!
//! A viewport is *attached* while it follows the bottom, which is what a
//! streaming turn needs. Scrolling up detaches it; scrolling back down to the
//! bottom re-attaches. Detachment is therefore never a mode the user has to
//! leave deliberately — reading back up and then returning is the whole
//! interaction, and a one-way door would be a worse bug than the one this fixes.
//!
//! # Two halves of one promise
//!
//! "Anchored to the bottom while a turn streams" is two claims, and #148's
//! Acceptance only names one of them. Appending while **detached** must not move
//! the offset — and appending while **attached** must follow the new bottom. A
//! viewport that never followed would satisfy the first and be useless, so both
//! are asserted here.

/// A window over rendered lines: where it starts, and whether it is following
/// the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    /// Index of the first visible line.
    offset: usize,
    /// Following the bottom as lines arrive.
    attached: bool,
}

impl Default for Viewport {
    /// Attached, at the top — which is also the bottom until there are more
    /// lines than rows.
    fn default() -> Self {
        Self {
            offset: 0,
            attached: true,
        }
    }
}

impl Viewport {
    /// The largest offset that still fills the pane.
    ///
    /// Scrolling past this would show blank rows below the last line, which
    /// reads as "the transcript ended" rather than "you scrolled too far".
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

    /// How many lines sit below the window — what the detach marker counts.
    ///
    /// Zero while attached, by construction rather than by a separate branch:
    /// an attached viewport is at `max_offset`.
    #[must_use]
    pub const fn below(self, total: usize, height: usize) -> usize {
        Self::max_offset(total, height).saturating_sub(self.offset)
    }

    /// Re-clamp after the line count or the pane size changed.
    ///
    /// Call this whenever lines are appended or the terminal is resized. While
    /// attached it follows the new bottom; while detached it holds position,
    /// except that it cannot hold an offset the shorter transcript no longer
    /// has — a resize that makes the pane taller must not leave blank rows
    /// below the end.
    #[must_use]
    pub const fn reflow(mut self, total: usize, height: usize) -> Self {
        let max = Self::max_offset(total, height);
        if self.attached {
            self.offset = max;
        } else if self.offset > max {
            self.offset = max;
            // Clamped to the bottom *is* the bottom, so it re-attaches rather
            // than sitting detached at a position indistinguishable from
            // attached — two states that look identical are a bug waiting for
            // the next append.
            self.attached = true;
        }
        self
    }

    /// Scroll the least that brings lines `[start, start + rows)` on screen.
    ///
    /// What a moving cursor needs (#155): selecting a block the pane is not
    /// showing selects something the user cannot see, which is worse than
    /// having no cursor at all. **The least** matters — jumping the block to
    /// the top or the middle would throw away the context around it that the
    /// user is reading it in.
    ///
    /// A block taller than the pane is shown from its **top**, since the two
    /// bounds cannot both be met and the top is where a block says what it is.
    #[must_use]
    pub const fn reveal(mut self, start: usize, rows: usize, total: usize, height: usize) -> Self {
        let max = Self::max_offset(total, height);
        let end = start + rows;
        if start < self.offset {
            self.offset = start;
        } else if end > self.offset + height {
            // Saturating rather than clamped to `max`: `total` counts the whole
            // transcript, so a block inside it can never need an offset past
            // the bottom, and clamping here would hide an arithmetic mistake
            // rather than show it.
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

    /// A full page up. `PgUp`.
    #[must_use]
    pub const fn page_up(self, height: usize) -> Self {
        self.up(height)
    }

    /// A full page down. `PgDn`.
    #[must_use]
    pub const fn page_down(self, total: usize, height: usize) -> Self {
        self.down(height, total, height)
    }

    /// Half a page up. `Ctrl+U`, which reaches here only when the composer is
    /// empty — #150 does not consume it then, deliberately.
    #[must_use]
    pub const fn half_up(self, height: usize) -> Self {
        self.up(height / 2)
    }

    /// Half a page down. `Ctrl+D`.
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

    /// The requirement the whole slice exists for.
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

    /// The half #148's Acceptance does not name. Without it a viewport that
    /// never follows would pass every other test here and be useless.
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

        // Explicitly.
        assert!(detached.to_bottom(100, H).attached());

        // And by scrolling: arriving at the bottom is the same thing as asking
        // for it, or detachment becomes a door that only closes one way.
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
        // The terminal is resized taller than everything left below.
        let v = detached.reflow(100, 200);
        assert_eq!(v.offset(), 0);
        assert!(
            v.attached(),
            "clamped to the bottom is the bottom; two states that look \
             identical would diverge on the next append"
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

    /// The scroll a moving cursor needs (#155): the least that shows the block.
    #[test]
    fn reveal_scrolls_the_least_that_brings_a_block_on_screen() {
        // A 100-line transcript in a 10-row pane, sitting at the bottom.
        let bottom = Viewport::default().reflow(100, 10);
        assert_eq!(bottom.offset(), 90);

        // Already on screen: nothing moves, and it stays attached.
        assert_eq!(bottom.reveal(92, 3, 100, 10), bottom);

        // Above the window: the top of the block becomes the top of the pane,
        // and no further — scrolling past it would throw away the context the
        // user is reading it in.
        let up = bottom.reveal(20, 3, 100, 10);
        assert_eq!(up.offset(), 20);
        assert!(!up.attached(), "scrolling back detaches");

        // Below the window: it comes to the *bottom* row, not the top.
        let down = up.reveal(40, 2, 100, 10);
        assert_eq!(down.offset(), 32, "the block's last line is the last row");
        assert!(!down.attached());

        // The newest block pulls the view back to the bottom, and re-attaches
        // there, because an offset at `max` is the bottom by definition.
        let back = up.reveal(97, 3, 100, 10);
        assert_eq!(back.offset(), 90);
        assert!(back.attached(), "arriving at the bottom re-attaches");
    }

    /// Both bounds cannot be met, so the one that says what the block *is* wins.
    #[test]
    fn a_block_taller_than_the_pane_is_shown_from_its_top() {
        let view = Viewport::default().reflow(100, 10).reveal(30, 40, 100, 10);
        assert_eq!(view.offset(), 30, "its head row is where the name is");
    }
}
