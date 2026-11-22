//! The way back: the pages you came through ("Frame and navigation" in
//! docs/internals/desktop.md). Every click that goes deeper opens a page,
//! and Back returns to the one before exactly as it was left. Here a page
//! is kept whole, its entity and all, rather than as a scroll position and
//! a memo: going back draws it again at once.

/// The tabs of the title bar, in their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Today,
    Library,
    Playlists,
    History,
    Devices,
}

impl Tab {
    pub(crate) const ALL: [Self; 5] = [
        Self::Today,
        Self::Library,
        Self::Playlists,
        Self::History,
        Self::Devices,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Today => "Today",
            Self::Library => "Library",
            Self::Playlists => "Playlists",
            Self::History => "History",
            Self::Devices => "Devices",
        }
    }
}

/// What a page is, for knowing whether a place is the one already open.
pub(crate) trait Place {
    /// The tab the page is under: an album's is the Library, however it was reached.
    fn tab(&self) -> Tab;
    /// Whether it is the same page as `other`.
    fn same(&self, other: &Self) -> bool;
}

/// The most pages kept to go back to. Older ones are forgotten.
pub(crate) const DEPTH: usize = 100;

/// The page open and the pages behind it.
#[derive(Debug)]
pub(crate) struct History<P> {
    behind: Vec<P>,
    current: P,
}

impl<P: Place> History<P> {
    pub(crate) fn new(first: P) -> Self {
        Self {
            behind: Vec::new(),
            current: first,
        }
    }

    pub(crate) fn current(&self) -> &P {
        &self.current
    }

    pub(crate) fn can_go_back(&self) -> bool {
        !self.behind.is_empty()
    }

    /// The page Back would go to.
    pub(crate) fn previous(&self) -> Option<&P> {
        self.behind.last()
    }

    /// Opens `place`, keeping this page to come back to. The page already
    /// open opens nothing: `false`.
    pub(crate) fn go(&mut self, place: P) -> bool {
        if place.same(&self.current) {
            return false;
        }
        let left = std::mem::replace(&mut self.current, place);
        self.behind.push(left);
        if self.behind.len() > DEPTH {
            self.behind.remove(0);
        }
        true
    }

    /// Goes back one page, giving the page that was left. `None` when there is none.
    pub(crate) fn back(&mut self) -> Option<P> {
        let previous = self.behind.pop()?;
        Some(std::mem::replace(&mut self.current, previous))
    }
}

/// When a page kept whole reads the library again. On screen it reads each
/// change as it comes; behind another, kept for Back, it notes the change
/// and reads once when it is shown again, so that a long way back does not
/// read every page in it on every play.
#[derive(Debug, Default)]
pub(crate) struct Freshness {
    behind: bool,
    /// What changed while behind: `Some(true)` for plays alone.
    missed: Option<bool>,
}

impl Freshness {
    /// A change heard, of plays alone or not: what to read now, if anything.
    pub(crate) fn heard(&mut self, plays_only: bool) -> Option<bool> {
        if !self.behind {
            return Some(plays_only);
        }
        self.missed = Some(self.missed.map_or(plays_only, |was| was && plays_only));
        None
    }

    /// The page shown again, or put behind another: what to read now.
    pub(crate) fn shown(&mut self, shown: bool) -> Option<bool> {
        self.behind = !shown;
        if shown { self.missed.take() } else { None }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    enum Test {
        Tab(Tab),
        Album(u32),
    }

    impl Place for Test {
        fn tab(&self) -> Tab {
            match self {
                Self::Tab(tab) => *tab,
                Self::Album(_) => Tab::Library,
            }
        }
        fn same(&self, other: &Self) -> bool {
            self == other
        }
    }

    #[test]
    fn going_deeper_and_back_returns_through_the_pages_in_order() {
        let mut history = History::new(Test::Tab(Tab::Library));
        assert!(!history.can_go_back());
        assert!(history.go(Test::Album(1)));
        assert!(history.go(Test::Album(2)));
        assert_eq!(history.current().tab(), Tab::Library);
        assert_eq!(history.back(), Some(Test::Album(2)));
        assert_eq!(history.current(), &Test::Album(1));
        assert_eq!(history.back(), Some(Test::Album(1)));
        assert_eq!(history.current(), &Test::Tab(Tab::Library));
        assert_eq!(history.back(), None);
    }

    #[test]
    fn the_page_already_open_opens_nothing() {
        let mut history = History::new(Test::Tab(Tab::Library));
        assert!(!history.go(Test::Tab(Tab::Library)));
        assert!(history.go(Test::Tab(Tab::History)));
        assert!(!history.go(Test::Tab(Tab::History)));
        assert_eq!(history.previous(), Some(&Test::Tab(Tab::Library)));
    }

    #[test]
    fn a_page_behind_reads_once_when_it_is_shown_again() {
        let mut page = Freshness::default();
        assert_eq!(page.heard(true), Some(true));
        assert_eq!(page.shown(false), None);
        assert_eq!(page.heard(true), None);
        assert_eq!(page.heard(true), None);
        assert_eq!(page.shown(true), Some(true));
        assert_eq!(page.shown(true), None);
        assert_eq!(page.shown(false), None);
        assert_eq!(page.heard(true), None);
        assert_eq!(page.heard(false), None);
        assert_eq!(page.heard(true), None);
        assert_eq!(page.shown(true), Some(false));
        assert_eq!(page.heard(false), Some(false));
    }

    #[test]
    fn only_the_last_hundred_pages_are_kept() {
        let mut history = History::new(Test::Album(0));
        for page in 1..=150 {
            history.go(Test::Album(page));
        }
        let mut steps = 0;
        while history.back().is_some() {
            steps += 1;
        }
        assert_eq!(steps, DEPTH);
        assert_eq!(history.current(), &Test::Album(50));
    }
}
