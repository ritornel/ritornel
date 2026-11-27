//! The Library's views ("Library views" in docs/internals/desktop.md).
//!
//! The Library has no fixed walls. It holds an ordered list of views, each
//! a small piece of configuration: what it lists, which of them, in what
//! order and under what headings, and how the rows are drawn. A view is
//! turned into a query for the server (`LibraryView::query`) and a shape
//! for the layout (`LibraryView::shape`); nothing else about it is special.
//!
//! The views are the app's own, kept beside what it remembers
//! (`remember.rs`) in `views.json`. A file that cannot be read is the
//! defaults; what a file says wrongly is put right as it is read, and what
//! it says that this build does not know is kept and written back.

use std::path::PathBuf;

use ritornel_contract::{
    FILTER_VERSION, Filter, GroupBy, ListKind, Predicate, Query, Sort, SortKey,
};
use serde_json::{Map, Value, json};

use crate::layout::{Metrics, Shape};

/// The version of the file.
const VERSION: u64 = 1;

/// A choice with a name the file knows it by and a label a person reads.
macro_rules! choices {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $id:literal, $label:literal;)+ }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub(crate) enum $name {
            $($variant,)+
        }

        impl $name {
            /// What the file calls it.
            pub(crate) const fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $id,)+
                }
            }

            /// What a person reads.
            pub(crate) const fn label(self) -> &'static str {
                match self {
                    $(Self::$variant => $label,)+
                }
            }

            /// The one the file calls `name`.
            pub(crate) fn named(name: &str) -> Option<Self> {
                match name {
                    $($id => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

choices! {
    /// What a view lists.
    Entity {
        Artists => "artists", "Artists";
        Albums => "albums", "Albums";
        Songs => "songs", "Songs";
    }
}

choices! {
    /// The order a view is in.
    ViewSort {
        Name => "name", "Name";
        Artist => "artist", "Artist";
        Title => "title", "Title";
        Album => "album", "Album";
        Year => "year", "Year";
        Added => "added", "Recently added";
        Plays => "plays", "Most played";
        LastPlayed => "last_played", "Recently played";
        Rating => "rating", "Rating";
    }
}

choices! {
    /// The headings a view is under.
    Grouping {
        Auto => "auto", "Auto";
        Letters => "letters", "Letters";
        Headings => "headings", "Headings";
        Year => "year", "Year";
        Decade => "decade", "Decade";
        Added => "added", "Added";
        Quality => "quality", "Quality";
        Shelf => "shelf", "Kind";
        None => "none", "None";
    }
}

choices! {
    /// How a view's rows are drawn.
    Presentation {
        Wall => "wall", "Wall";
        List => "list", "List";
        Table => "table", "Table";
    }
}

choices! {
    /// How much room each row is given.
    Density {
        Compact => "compact", "Compact";
        Comfortable => "comfortable", "Comfortable";
        Roomy => "roomy", "Roomy";
    }
}

choices! {
    /// One thing a view can be narrowed to.
    Narrow {
        FullAlbums => "full_albums", "Full albums (3+)";
        Unplayed => "unplayed", "Unplayed";
        Played => "played", "Played";
        Rated => "rated_4_plus", "4+ stars";
        Lossless => "lossless", "Lossless";
        Lossy => "lossy", "Lossy";
        Loose => "loose", "Loose tracks";
    }
}

impl Entity {
    pub(crate) const ALL: [Self; 3] = [Self::Artists, Self::Albums, Self::Songs];

    pub(crate) const fn kind(self) -> ListKind {
        match self {
            Self::Artists => ListKind::Artists,
            Self::Albums => ListKind::Albums,
            Self::Songs => ListKind::Songs,
        }
    }

    /// A count of it: "1 album", "8,653 albums".
    pub(crate) fn count(self, total: usize) -> String {
        let (one, many) = match self {
            Self::Artists => ("artist", "artists"),
            Self::Albums => ("album", "albums"),
            Self::Songs => ("song", "songs"),
        };
        format!(
            "{} {}",
            crate::wall::thousands(total),
            if total == 1 { one } else { many }
        )
    }

    /// The order it opens in, and takes when a view names one it cannot.
    pub(crate) const fn first_sort(self) -> ViewSort {
        match self {
            Self::Artists => ViewSort::Name,
            Self::Albums => ViewSort::Artist,
            Self::Songs => ViewSort::Title,
        }
    }

    /// The orders it can be in, its first among them.
    pub(crate) const fn sorts(self) -> &'static [ViewSort] {
        match self {
            Self::Artists => &[ViewSort::Name, ViewSort::Plays, ViewSort::LastPlayed],
            Self::Albums => &[
                ViewSort::Artist,
                ViewSort::Title,
                ViewSort::Year,
                ViewSort::Added,
                ViewSort::Plays,
                ViewSort::LastPlayed,
            ],
            Self::Songs => &[
                ViewSort::Title,
                ViewSort::Album,
                ViewSort::Plays,
                ViewSort::LastPlayed,
                ViewSort::Rating,
            ],
        }
    }

    /// The headings it can be under.
    pub(crate) const fn groupings(self) -> &'static [Grouping] {
        match self {
            Self::Artists => &[
                Grouping::Auto,
                Grouping::Letters,
                Grouping::Headings,
                Grouping::None,
            ],
            Self::Albums => &[
                Grouping::Auto,
                Grouping::Letters,
                Grouping::Headings,
                Grouping::Year,
                Grouping::Decade,
                Grouping::Added,
                Grouping::Quality,
                Grouping::Shelf,
                Grouping::None,
            ],
            Self::Songs => &[
                Grouping::Auto,
                Grouping::Letters,
                Grouping::Headings,
                Grouping::Year,
                Grouping::Decade,
                Grouping::Added,
                Grouping::Quality,
                Grouping::None,
            ],
        }
    }

    /// What it can be narrowed to.
    pub(crate) const fn narrows(self) -> &'static [Narrow] {
        match self {
            Self::Artists => &[Narrow::Unplayed, Narrow::Played],
            Self::Albums => &[
                Narrow::FullAlbums,
                Narrow::Unplayed,
                Narrow::Played,
                Narrow::Lossless,
                Narrow::Lossy,
            ],
            Self::Songs => &[
                Narrow::Unplayed,
                Narrow::Played,
                Narrow::Rated,
                Narrow::Lossless,
                Narrow::Lossy,
                Narrow::Loose,
            ],
        }
    }

    /// Whether its rows have a year to be narrowed by.
    pub(crate) const fn has_years(self) -> bool {
        !matches!(self, Self::Artists)
    }
}

impl ViewSort {
    pub(crate) const fn key(self) -> SortKey {
        match self {
            Self::Name => SortKey::Name,
            Self::Artist => SortKey::Artist,
            Self::Title => SortKey::Title,
            Self::Album => SortKey::Album,
            Self::Year => SortKey::Year,
            Self::Added => SortKey::Added,
            Self::Plays => SortKey::Plays,
            Self::LastPlayed => SortKey::LastPlayed,
            Self::Rating => SortKey::Rating,
        }
    }
}

impl Grouping {
    /// What the query asks for: `None` leaves the sections to the order.
    pub(crate) const fn group_by(self) -> Option<GroupBy> {
        match self {
            Self::Auto => None,
            Self::Letters | Self::Headings => Some(GroupBy::Letter),
            Self::Year => Some(GroupBy::Year),
            Self::Decade => Some(GroupBy::Decade),
            Self::Added => Some(GroupBy::Added),
            Self::Quality => Some(GroupBy::Quality),
            Self::Shelf => Some(GroupBy::Shelf),
            Self::None => Some(GroupBy::None),
        }
    }
}

impl Presentation {
    pub(crate) const ALL: [Self; 3] = [Self::Wall, Self::List, Self::Table];

    /// Its icon.
    pub(crate) const fn icon(self) -> &'static str {
        match self {
            Self::Wall => "viewWall",
            Self::List => "viewList",
            Self::Table => "viewTable",
        }
    }
}

impl Density {
    pub(crate) const ALL: [Self; 3] = [Self::Compact, Self::Comfortable, Self::Roomy];
}

impl Narrow {
    /// The one it cannot hold with: a view is not both.
    pub(crate) const fn excludes(self) -> Option<Self> {
        match self {
            Self::Unplayed => Some(Self::Played),
            Self::Played => Some(Self::Unplayed),
            Self::Lossless => Some(Self::Lossy),
            Self::Lossy => Some(Self::Lossless),
            Self::FullAlbums | Self::Rated | Self::Loose => None,
        }
    }

    fn predicate(self) -> Predicate {
        match self {
            Self::FullAlbums => Predicate::MinTracks { count: 3 },
            Self::Unplayed => Predicate::PlayCount {
                min: None,
                max: Some(0),
            },
            Self::Played => Predicate::PlayCount {
                min: Some(1),
                max: None,
            },
            Self::Rated => Predicate::MinRating { rating: 80 },
            Self::Lossless => Predicate::Lossless { lossless: true },
            Self::Lossy => Predicate::Lossless { lossless: false },
            Self::Loose => Predicate::NoRecord,
        }
    }
}

/// Which rows a view has.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Filters {
    /// What it is narrowed to, every one holding.
    pub narrow: Vec<Narrow>,
    /// The years it is from, either end of which may be open.
    pub year_from: Option<i64>,
    pub year_to: Option<i64>,
    /// What a row's title, artist or album must contain.
    pub text: Option<String>,
}

impl Filters {
    pub(crate) fn is_empty(&self) -> bool {
        self.narrow.is_empty()
            && self.year_from.is_none()
            && self.year_to.is_none()
            && self.text.is_none()
    }

    pub(crate) fn has(&self, narrow: Narrow) -> bool {
        self.narrow.contains(&narrow)
    }

    /// Turns one on or off; one turned on turns off the one it cannot hold
    /// with.
    pub(crate) fn toggle(&mut self, narrow: Narrow) {
        if self.has(narrow) {
            self.narrow.retain(|each| *each != narrow);
        } else {
            self.narrow.retain(|each| Some(*each) != narrow.excludes());
            self.narrow.push(narrow);
        }
    }
}

/// One view of the Library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LibraryView {
    /// What the file and a script know it by. Never shown.
    pub id: String,
    pub name: String,
    pub entity: Entity,
    pub filter: Filters,
    pub grouping: Grouping,
    pub sort: ViewSort,
    pub presentation: Presentation,
    pub density: Density,
    /// What the file said of it that this build does not know, written
    /// back as it was.
    extra: Map<String, Value>,
}

impl LibraryView {
    fn new(id: &str, name: &str, entity: Entity) -> Self {
        Self {
            id: id.to_owned(),
            name: name.to_owned(),
            entity,
            filter: Filters::default(),
            grouping: Grouping::Auto,
            sort: entity.first_sort(),
            presentation: Presentation::Wall,
            density: Density::Comfortable,
            extra: Map::new(),
        }
    }

    /// The view with everything its entity does not take put right: an
    /// order or headings it cannot have become its own, what it cannot be
    /// narrowed to is dropped, and so is one of two that cannot both hold.
    /// Its name is left as it is, since it may be half typed.
    pub(crate) fn sanitized(mut self) -> Self {
        let entity = self.entity;
        if !entity.sorts().contains(&self.sort) {
            self.sort = entity.first_sort();
        }
        if !entity.groupings().contains(&self.grouping) {
            self.grouping = Grouping::Auto;
        }
        let mut kept: Vec<Narrow> = Vec::new();
        for narrow in std::mem::take(&mut self.filter.narrow) {
            let allowed = entity.narrows().contains(&narrow);
            let clashes = narrow.excludes().is_some_and(|other| kept.contains(&other));
            if allowed && !clashes && !kept.contains(&narrow) {
                kept.push(narrow);
            }
        }
        self.filter.narrow = kept;
        if !entity.has_years() {
            self.filter.year_from = None;
            self.filter.year_to = None;
        }
        if let (Some(from), Some(to)) = (self.filter.year_from, self.filter.year_to)
            && from > to
        {
            (self.filter.year_from, self.filter.year_to) = (Some(to), Some(from));
        }
        self.filter.text = self
            .filter
            .text
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty());
        self
    }

    /// What it is called where its name is shown: its name, or with none,
    /// as while one is being typed, what it lists.
    pub(crate) fn title(&self) -> &str {
        if self.name.trim().is_empty() {
            self.entity.label()
        } else {
            &self.name
        }
    }

    /// What the server is asked for.
    pub(crate) fn query(&self) -> Query {
        let mut of: Vec<Predicate> = self
            .filter
            .narrow
            .iter()
            .map(|narrow| narrow.predicate())
            .collect();
        if self.filter.year_from.is_some() || self.filter.year_to.is_some() {
            of.push(Predicate::YearRange {
                from: self.filter.year_from,
                to: self.filter.year_to,
            });
        }
        let predicate = if of.len() > 1 {
            Some(Predicate::All { of })
        } else {
            of.pop()
        };
        Query {
            text: self.filter.text.clone(),
            filter: predicate.map(|predicate| Filter {
                version: FILTER_VERSION,
                predicate,
            }),
            sort: Some(vec![Sort {
                key: self.sort.key(),
            }]),
            group_by: self.grouping.group_by(),
        }
    }

    /// How it is laid out, given the sections the server answered with:
    /// flat with none; under letters in their lane or under small headings
    /// when the sections are letters, as the view says or, left to itself,
    /// as Settings › Appearance does (`large_letters`); and under larger
    /// headings for anything else, a year or a stretch of time. A table
    /// has no lane.
    pub(crate) fn shape(&self, sections: &[(String, usize)], large_letters: bool) -> Shape {
        if sections.is_empty() {
            return Shape::Flat;
        }
        let letters = sections.iter().all(|(label, _)| {
            let mut characters = label.chars();
            matches!(
                (characters.next(), characters.next()),
                (Some('A'..='Z' | '#'), None)
            )
        });
        if !letters {
            return Shape::Dated;
        }
        let large = match self.grouping {
            Grouping::Letters => true,
            Grouping::Headings => false,
            _ => large_letters,
        };
        if large && self.presentation != Presentation::Table {
            Shape::Lettered
        } else {
            Shape::Headed
        }
    }

    /// What its rows measure.
    pub(crate) fn metrics(&self) -> Metrics {
        let by_density = |sizes: [f32; 3]| match self.density {
            Density::Compact => sizes[0],
            Density::Comfortable => sizes[1],
            Density::Roomy => sizes[2],
        };
        match self.presentation {
            Presentation::Wall => Metrics::Grid {
                tile: by_density([124., 150., 184.]),
            },
            Presentation::List => Metrics::Rows {
                row: by_density([48., 58., 70.]),
            },
            Presentation::Table => Metrics::Rows {
                row: by_density([30., 36., 44.]),
            },
        }
    }

    /// How large a row's picture is drawn, in points.
    pub(crate) fn art_side(&self) -> f32 {
        match (self.presentation, self.metrics()) {
            (Presentation::Wall, Metrics::Grid { tile }) => tile,
            (Presentation::List, Metrics::Rows { row }) => row - 12.,
            (_, Metrics::Rows { row }) => row - 10.,
            (_, Metrics::Grid { tile }) => tile,
        }
    }

    fn read(value: &Value) -> Option<Self> {
        let said = value.as_object()?;
        let text = |key: &str| said.get(key).and_then(Value::as_str);
        let entity = text("entity").and_then(Entity::named)?;
        let id = text("id").filter(|id| !id.trim().is_empty())?;
        let name = text("name").filter(|name| !name.trim().is_empty());
        let mut view = Self::new(id, name.unwrap_or(entity.label()), entity);
        if let Some(sort) = text("sort").and_then(ViewSort::named) {
            view.sort = sort;
        }
        if let Some(grouping) = text("grouping").and_then(Grouping::named) {
            view.grouping = grouping;
        }
        if let Some(presentation) = text("presentation").and_then(Presentation::named) {
            view.presentation = presentation;
        }
        if let Some(density) = text("density").and_then(Density::named) {
            view.density = density;
        }
        if let Some(filter) = said.get("filter").and_then(Value::as_object) {
            view.filter.narrow = filter
                .get("is")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|each| each.as_str().and_then(Narrow::named))
                .collect();
            view.filter.year_from = filter.get("yearFrom").and_then(Value::as_i64);
            view.filter.year_to = filter.get("yearTo").and_then(Value::as_i64);
            view.filter.text = filter
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        view.extra = said
            .iter()
            .filter(|(key, _)| !KNOWN.contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        Some(view.sanitized())
    }

    fn written(&self) -> Value {
        let mut filter = Map::new();
        if !self.filter.narrow.is_empty() {
            let narrow: Vec<&str> = self.filter.narrow.iter().map(|each| each.name()).collect();
            filter.insert("is".to_owned(), json!(narrow));
        }
        if let Some(from) = self.filter.year_from {
            filter.insert("yearFrom".to_owned(), json!(from));
        }
        if let Some(to) = self.filter.year_to {
            filter.insert("yearTo".to_owned(), json!(to));
        }
        if let Some(text) = &self.filter.text {
            filter.insert("text".to_owned(), json!(text));
        }
        let mut written = self.extra.clone();
        for (key, value) in [
            ("id", json!(self.id)),
            ("name", json!(self.name)),
            ("entity", json!(self.entity.name())),
            ("filter", Value::Object(filter)),
            ("grouping", json!(self.grouping.name())),
            ("sort", json!(self.sort.name())),
            ("presentation", json!(self.presentation.name())),
            ("density", json!(self.density.name())),
        ] {
            written.insert(key.to_owned(), value);
        }
        Value::Object(written)
    }
}

/// The keys of a view this build reads.
const KNOWN: &[&str] = &[
    "id",
    "name",
    "entity",
    "filter",
    "grouping",
    "sort",
    "presentation",
    "density",
];

/// A view ready made, to start from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Template {
    /// What a script knows it by.
    pub id: &'static str,
    pub name: &'static str,
    entity: Entity,
    narrow: &'static [Narrow],
    sort: ViewSort,
    grouping: Grouping,
    presentation: Presentation,
    density: Density,
}

impl Template {
    pub(crate) fn view(&self) -> LibraryView {
        LibraryView {
            filter: Filters {
                narrow: self.narrow.to_vec(),
                ..Filters::default()
            },
            grouping: self.grouping,
            sort: self.sort,
            presentation: self.presentation,
            density: self.density,
            ..LibraryView::new(self.id, self.name, self.entity)
        }
        .sanitized()
    }
}

/// The views the Library opens with.
const DEFAULTS: [Template; 3] = [
    Template {
        id: "artists",
        name: "Artists",
        entity: Entity::Artists,
        narrow: &[],
        sort: ViewSort::Name,
        grouping: Grouping::Auto,
        presentation: Presentation::Wall,
        density: Density::Comfortable,
    },
    Template {
        id: "albums",
        name: "Albums",
        entity: Entity::Albums,
        narrow: &[],
        sort: ViewSort::Artist,
        grouping: Grouping::Auto,
        presentation: Presentation::Wall,
        density: Density::Comfortable,
    },
    Template {
        id: "songs",
        name: "Songs",
        entity: Entity::Songs,
        narrow: &[],
        sort: ViewSort::Title,
        grouping: Grouping::Headings,
        presentation: Presentation::Table,
        density: Density::Comfortable,
    },
];

/// Views to add with one click, from the + beside the views.
pub(crate) const TEMPLATES: [Template; 8] = [
    Template {
        id: "full-albums",
        name: "Full Albums",
        entity: Entity::Albums,
        narrow: &[Narrow::FullAlbums],
        sort: ViewSort::Artist,
        grouping: Grouping::Auto,
        presentation: Presentation::Wall,
        density: Density::Comfortable,
    },
    Template {
        id: "albums-by-decade",
        name: "Albums by Decade",
        entity: Entity::Albums,
        narrow: &[],
        sort: ViewSort::Year,
        grouping: Grouping::Decade,
        presentation: Presentation::Wall,
        density: Density::Comfortable,
    },
    Template {
        id: "unplayed-albums",
        name: "Unplayed Albums",
        entity: Entity::Albums,
        narrow: &[Narrow::Unplayed],
        sort: ViewSort::Added,
        grouping: Grouping::Added,
        presentation: Presentation::Wall,
        density: Density::Comfortable,
    },
    Template {
        id: "recently-added",
        name: "Recently Added",
        entity: Entity::Albums,
        narrow: &[],
        sort: ViewSort::Added,
        grouping: Grouping::Added,
        presentation: Presentation::Wall,
        density: Density::Comfortable,
    },
    Template {
        id: "albums-table",
        name: "Albums Table",
        entity: Entity::Albums,
        narrow: &[],
        sort: ViewSort::Year,
        grouping: Grouping::None,
        presentation: Presentation::Table,
        density: Density::Compact,
    },
    Template {
        id: "most-played-songs",
        name: "Most Played Songs",
        entity: Entity::Songs,
        narrow: &[],
        sort: ViewSort::Plays,
        grouping: Grouping::None,
        presentation: Presentation::Table,
        density: Density::Comfortable,
    },
    Template {
        id: "loose-tracks",
        name: "Loose Tracks",
        entity: Entity::Songs,
        narrow: &[Narrow::Loose],
        sort: ViewSort::Title,
        grouping: Grouping::None,
        presentation: Presentation::Table,
        density: Density::Comfortable,
    },
    Template {
        id: "artists-directory",
        name: "Artists Directory",
        entity: Entity::Artists,
        narrow: &[],
        sort: ViewSort::Name,
        grouping: Grouping::Headings,
        presentation: Presentation::List,
        density: Density::Compact,
    },
];

/// The Library's views, in order, and the one on show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Views {
    /// The ID of the one on show.
    pub active: String,
    /// Never empty.
    pub views: Vec<LibraryView>,
    extra: Map<String, Value>,
}

impl Default for Views {
    fn default() -> Self {
        let views: Vec<LibraryView> = DEFAULTS.iter().map(Template::view).collect();
        Self {
            active: views
                .first()
                .map(|view| view.id.clone())
                .unwrap_or_default(),
            views,
            extra: Map::new(),
        }
    }
}

/// The view shown should the list ever be empty, which it never is: the
/// first the Library opens with.
fn first_view() -> &'static LibraryView {
    static FIRST: std::sync::OnceLock<LibraryView> = std::sync::OnceLock::new();
    FIRST.get_or_init(|| LibraryView::new("artists", "Artists", Entity::Artists))
}

/// The file's name, as the Library says where the views are kept.
pub(crate) const FILE: &str = "views.json";

/// Where the views are kept: `RITORNEL_VIEWS_PATH` when set, for trying
/// things out, else beside what the app remembers. In tests, nowhere, so
/// that a test never reads or writes this computer's.
fn path() -> Option<PathBuf> {
    if cfg!(test) {
        return None;
    }
    if let Some(path) = std::env::var_os("RITORNEL_VIEWS_PATH").filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }
    Some(
        crate::platform::settings_dir()?
            .join("dev.ritornel.desktop-gpui")
            .join(FILE),
    )
}

impl Views {
    /// The views as kept, or the defaults: opened on the wall, and in the
    /// order of albums, that the app remembered before it had views.
    pub(crate) fn load() -> Self {
        let read = path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        match read {
            Some(read) => Self::read_from(&read),
            None => {
                let remembered = crate::remember::load();
                let mut views = Self::default();
                if remembered.wall == "albums" {
                    views.active = "albums".to_owned();
                }
                if let (Some(albums), Some(sort)) = (
                    views.views.iter_mut().find(|view| view.id == "albums"),
                    ViewSort::named(&remembered.albums_sort),
                ) {
                    albums.sort = sort;
                }
                views
            }
        }
    }

    pub(crate) fn save(&self) {
        let Some(path) = path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let text = serde_json::to_string_pretty(&self.written()).unwrap_or_default();
        let _ = std::fs::write(path, text);
    }

    /// What a file says the views are. Views it says wrongly are put right
    /// or left out; with none left, they are the defaults.
    fn read_from(read: &Value) -> Self {
        let mut views: Vec<LibraryView> = Vec::new();
        for view in read
            .get("views")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(LibraryView::read)
        {
            // Two of one ID: the first stands.
            if !views.iter().any(|held| held.id == view.id) {
                views.push(view);
            }
        }
        let Some(first) = views.first() else {
            return Self::default();
        };
        let active = read
            .get("active")
            .and_then(Value::as_str)
            .filter(|active| views.iter().any(|view| view.id == *active))
            .map_or_else(|| first.id.clone(), str::to_owned);
        let extra = read
            .as_object()
            .into_iter()
            .flatten()
            .filter(|(key, _)| !["version", "active", "views"].contains(&key.as_str()))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        Self {
            active,
            views,
            extra,
        }
    }

    fn written(&self) -> Value {
        let mut written = self.extra.clone();
        written.insert("version".to_owned(), json!(VERSION));
        written.insert("active".to_owned(), json!(self.active));
        written.insert(
            "views".to_owned(),
            Value::Array(self.views.iter().map(LibraryView::written).collect()),
        );
        Value::Object(written)
    }

    /// Where the view on show is.
    pub(crate) fn active_index(&self) -> usize {
        self.views
            .iter()
            .position(|view| view.id == self.active)
            .unwrap_or(0)
    }

    /// The view on show.
    pub(crate) fn active(&self) -> &LibraryView {
        match self.views.get(self.active_index()) {
            Some(view) => view,
            None => first_view(),
        }
    }

    /// Changes the view on show, which is put right afterwards.
    pub(crate) fn change(&mut self, change: impl FnOnce(&mut LibraryView)) {
        let index = self.active_index();
        if let Some(view) = self.views.get_mut(index) {
            change(view);
            *view = view.clone().sanitized();
        }
    }

    /// Where the view with this ID is, or else the first of this name,
    /// whatever its case.
    pub(crate) fn find(&self, id_or_name: &str) -> Option<usize> {
        let wanted = id_or_name.trim();
        self.views
            .iter()
            .position(|view| view.id == wanted)
            .or_else(|| {
                self.views
                    .iter()
                    .position(|view| view.name.eq_ignore_ascii_case(wanted))
            })
    }

    /// An ID no view has, made from `name`.
    fn free_id(&self, name: &str) -> String {
        let mut slug = String::new();
        for character in name.chars() {
            if character.is_alphanumeric() {
                slug.extend(character.to_lowercase());
            } else if !slug.is_empty() && !slug.ends_with('-') {
                slug.push('-');
            }
        }
        let slug = slug.trim_end_matches('-');
        let slug = if slug.is_empty() { "view" } else { slug };
        let taken = |id: &str| self.views.iter().any(|view| view.id == id);
        if !taken(slug) {
            return slug.to_owned();
        }
        (2..)
            .map(|count| format!("{slug}-{count}"))
            .find(|id| !taken(id))
            .unwrap_or_default()
    }

    /// Adds a view after the others, under an ID of its own, and shows it.
    pub(crate) fn add(&mut self, mut view: LibraryView) {
        view.id = self.free_id(&view.name);
        self.active = view.id.clone();
        self.views.push(view.sanitized());
    }

    /// Takes the view on show away, and shows the one before it. The last
    /// view stays: the Library always has one.
    pub(crate) fn remove_active(&mut self) -> bool {
        if self.views.len() < 2 {
            return false;
        }
        let index = self.active_index();
        self.views.remove(index);
        if let Some(before) = self.views.get(index.saturating_sub(1)) {
            self.active = before.id.clone();
        }
        true
    }

    /// Moves the view on show one place left (-1) or right (1).
    pub(crate) fn move_active(&mut self, by: isize) -> bool {
        let index = self.active_index();
        let Some(to) = index
            .checked_add_signed(by)
            .filter(|to| *to < self.views.len())
        else {
            return false;
        };
        self.views.swap(index, to);
        true
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn the_library_opens_with_artists_albums_and_songs() {
        let views = Views::default();
        let said: Vec<_> = views
            .views
            .iter()
            .map(|view| {
                (
                    view.name.as_str(),
                    view.entity,
                    view.presentation,
                    view.sort,
                    view.grouping,
                )
            })
            .collect();
        assert_eq!(
            said,
            [
                (
                    "Artists",
                    Entity::Artists,
                    Presentation::Wall,
                    ViewSort::Name,
                    Grouping::Auto
                ),
                (
                    "Albums",
                    Entity::Albums,
                    Presentation::Wall,
                    ViewSort::Artist,
                    Grouping::Auto
                ),
                (
                    "Songs",
                    Entity::Songs,
                    Presentation::Table,
                    ViewSort::Title,
                    Grouping::Headings
                ),
            ]
        );
        assert_eq!(views.active().id, "artists");
    }

    #[test]
    fn every_template_is_a_view_its_entity_takes() {
        for template in TEMPLATES.iter().chain(&DEFAULTS) {
            let view = template.view();
            assert_eq!(view.clone().sanitized(), view, "{}", template.id);
            assert_eq!(view.filter.narrow, template.narrow, "{}", template.id);
            assert_eq!(view.sort, template.sort, "{}", template.id);
            assert_eq!(view.grouping, template.grouping, "{}", template.id);
            // And what the server is asked for is what its kind supports.
            let kind = view.entity.kind();
            let query = view.query();
            assert!(kind.sorts().contains(&view.sort.key()), "{}", template.id);
            if let Some(group) = query.group_by {
                assert!(kind.groupings().contains(&group), "{}", template.id);
            }
        }
        // Whatever a person can choose for an entity, its kind supports.
        for entity in Entity::ALL {
            let kind = entity.kind();
            for sort in entity.sorts() {
                assert!(kind.sorts().contains(&sort.key()), "{entity:?} {sort:?}");
            }
            for grouping in entity.groupings() {
                if let Some(group) = grouping.group_by() {
                    assert!(kind.groupings().contains(&group), "{entity:?}");
                }
            }
            for narrow in entity.narrows() {
                assert!(
                    kind.predicates().contains(&narrow.predicate().name()),
                    "{entity:?} {narrow:?}"
                );
            }
        }
    }

    #[test]
    fn a_view_is_a_query_of_its_order_its_headings_and_what_it_is_narrowed_to() {
        let mut view = TEMPLATES[0].view();
        let query = view.query();
        assert_eq!(
            query.filter.map(|filter| filter.predicate),
            Some(Predicate::MinTracks { count: 3 })
        );
        assert_eq!(query.group_by, None);
        assert_eq!(
            query.sort,
            Some(vec![Sort {
                key: SortKey::Artist
            }])
        );

        view.filter.toggle(Narrow::Lossless);
        view.filter.year_from = Some(1990);
        view.filter.text = Some("night".to_owned());
        view.grouping = Grouping::Decade;
        let query = view.query();
        assert_eq!(
            query.filter.map(|filter| filter.predicate),
            Some(Predicate::All {
                of: vec![
                    Predicate::MinTracks { count: 3 },
                    Predicate::Lossless { lossless: true },
                    Predicate::YearRange {
                        from: Some(1990),
                        to: None
                    },
                ]
            })
        );
        assert_eq!(query.group_by, Some(GroupBy::Decade));
        assert_eq!(query.text.as_deref(), Some("night"));
        // No headings at all is asked for as such, and plain views ask for
        // no filter.
        assert_eq!(TEMPLATES[4].view().query().group_by, Some(GroupBy::None));
        assert_eq!(DEFAULTS[1].view().query().filter, None);
    }

    #[test]
    fn what_an_entity_does_not_take_is_put_right() {
        let mut view = TEMPLATES[0].view();
        view.filter.narrow = vec![
            Narrow::Played,
            Narrow::Unplayed,
            Narrow::FullAlbums,
            Narrow::Rated,
            Narrow::Played,
        ];
        view.sort = ViewSort::Rating;
        view.grouping = Grouping::Shelf;
        view.filter.year_from = Some(2001);
        view.filter.year_to = Some(1990);
        view.filter.text = Some("   ".to_owned());
        view.name = " ".to_owned();
        assert_eq!(view.title(), "Albums");
        let albums = view.clone().sanitized();
        // One of two that cannot both hold, nothing songs alone have, and
        // nothing twice.
        assert_eq!(albums.filter.narrow, [Narrow::Played, Narrow::FullAlbums]);
        assert_eq!(albums.sort, ViewSort::Artist);
        assert_eq!(albums.grouping, Grouping::Shelf);
        assert_eq!(
            (albums.filter.year_from, albums.filter.year_to),
            (Some(1990), Some(2001))
        );
        assert_eq!(albums.filter.text, None);

        view.entity = Entity::Artists;
        let artists = view.sanitized();
        assert_eq!(artists.filter.narrow, [Narrow::Played]);
        assert_eq!(artists.sort, ViewSort::Name);
        assert_eq!(artists.grouping, Grouping::Auto);
        assert_eq!(
            (artists.filter.year_from, artists.filter.year_to),
            (None, None)
        );
    }

    #[test]
    fn narrowing_to_one_of_a_pair_lets_go_of_the_other() {
        let mut filters = Filters::default();
        filters.toggle(Narrow::Unplayed);
        filters.toggle(Narrow::Lossless);
        filters.toggle(Narrow::Played);
        assert_eq!(filters.narrow, [Narrow::Lossless, Narrow::Played]);
        filters.toggle(Narrow::Played);
        assert_eq!(filters.narrow, [Narrow::Lossless]);
        assert!(!filters.is_empty());
    }

    #[test]
    fn the_file_is_read_back_as_written_and_keeps_what_it_does_not_know() {
        let mut views = Views::default();
        views.add(TEMPLATES[1].view());
        views.change(|view| view.filter.year_to = Some(1999));
        let written = views.written();
        assert_eq!(written["version"], 1);
        assert_eq!(written["active"], "albums-by-decade");
        assert_eq!(Views::read_from(&written), views);

        // A later build's fields, on the file and on a view, are kept.
        let mut later = written.clone();
        later["theme"] = json!("dusk");
        later["views"][0]["columns"] = json!(["title", "plays"]);
        let read = Views::read_from(&later);
        assert_eq!(read.views, {
            let mut expected = views.views.clone();
            expected[0]
                .extra
                .insert("columns".to_owned(), json!(["title", "plays"]));
            expected
        });
        let again = read.written();
        assert_eq!(again["theme"], "dusk");
        assert_eq!(again["views"][0]["columns"], json!(["title", "plays"]));
    }

    #[test]
    fn a_file_that_says_things_wrongly_is_put_right_or_is_the_defaults() {
        let read = Views::read_from(&json!({
            "version": 1,
            "active": "gone",
            "views": [
                { "id": "a", "name": "A", "entity": "albums", "sort": "rating",
                  "grouping": "sideways", "presentation": "hologram", "density": 4,
                  "filter": { "is": ["lossy", "lossless", "loose", "nonsense"], "yearFrom": "x" } },
                { "id": "a", "name": "Again", "entity": "songs" },
                { "id": "b", "entity": "playlists" },
                { "name": "No ID", "entity": "albums" },
                "nonsense",
                { "id": "s", "entity": "songs", "presentation": "table" }
            ]
        }));
        assert_eq!(read.views.len(), 2);
        let first = &read.views[0];
        assert_eq!(
            (
                first.sort,
                first.grouping,
                first.presentation,
                first.density
            ),
            (
                ViewSort::Artist,
                Grouping::Auto,
                Presentation::Wall,
                Density::Comfortable
            )
        );
        assert_eq!(first.filter.narrow, [Narrow::Lossy]);
        assert_eq!(first.filter.year_from, None);
        // A view with no name takes its entity's, and the one on show is
        // one there is.
        assert_eq!(read.views[1].name, "Songs");
        assert_eq!(read.active, "a");
        for nonsense in [json!(null), json!({ "views": [] }), json!([1, 2])] {
            assert_eq!(Views::read_from(&nonsense), Views::default());
        }
    }

    #[test]
    fn views_are_added_copied_moved_and_taken_away() {
        let mut views = Views {
            active: "albums".to_owned(),
            ..Views::default()
        };
        let mut copy = views.active().clone();
        copy.name = "Albums copy".to_owned();
        views.add(copy);
        assert_eq!(views.active().name, "Albums copy");
        assert_eq!(views.active().id, "albums-copy");
        views.add(DEFAULTS[1].view());
        assert_eq!(views.active().id, "albums-2");
        assert_eq!(views.views.len(), 5);
        assert_eq!(views.find("ALBUMS COPY"), Some(3));
        assert_eq!(views.find("songs"), Some(2));
        assert_eq!(views.find("nowhere"), None);

        assert!(!views.move_active(1));
        assert!(views.move_active(-1));
        assert_eq!(views.active_index(), 3);
        assert!(views.remove_active());
        assert_eq!(views.active().id, "songs");
        while views.remove_active() {}
        assert_eq!(views.views.len(), 1);
        assert!(!views.move_active(-1));
    }

    #[test]
    fn a_view_is_shaped_by_its_sections_and_measured_by_its_density() {
        let letters = [("A".to_owned(), 0), ("#".to_owned(), 9)];
        let years = [("1990s".to_owned(), 0)];
        let mut view = DEFAULTS[1].view();
        assert_eq!(view.shape(&letters, true), Shape::Lettered);
        assert_eq!(view.shape(&letters, false), Shape::Headed);
        assert_eq!(view.shape(&years, true), Shape::Dated);
        assert_eq!(view.shape(&[], true), Shape::Flat);
        view.grouping = Grouping::Headings;
        assert_eq!(view.shape(&letters, true), Shape::Headed);
        view.grouping = Grouping::Letters;
        assert_eq!(view.shape(&letters, false), Shape::Lettered);
        view.presentation = Presentation::Table;
        assert_eq!(view.shape(&letters, true), Shape::Headed);

        view.presentation = Presentation::Wall;
        let tile = |view: &LibraryView| match view.metrics() {
            Metrics::Grid { tile } => tile,
            Metrics::Rows { .. } => 0.,
        };
        view.density = Density::Compact;
        assert_eq!(tile(&view), 124.);
        view.density = Density::Roomy;
        assert_eq!(tile(&view), 184.);
        view.presentation = Presentation::Table;
        assert!(matches!(view.metrics(), Metrics::Rows { row } if row == 44.));
        assert!(view.art_side() < 44.);
    }
}
