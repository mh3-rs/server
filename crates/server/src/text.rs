//! Texts the consoles show, and the terms.
//!
//! Texts are a dictionary in the code: [`text`] returns English for every [`Text`], so a missing text is a compile
//! error. Overrides and translations come with the website (M8) by changing only that function's body.
//! Terms are overridable already (the `terms` table): an edit must change the version the console remembers.
//! Markup is the game's: `<BR>`, `<BODY>`, `<CENTER>`, `<LEFT>`, `<SIZE=n>`, `<C=n>` (colour), `<END>`.

use std::borrow::Cow;

use sqlx::SqlitePool;

/// The console's language. English only until translations come (M8); the console sends its own in
/// `AnsConnection`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    English,
}

impl Language {
    /// ISO 639-1, as in the `terms` table.
    pub fn code(self) -> &'static str {
        match self {
            Language::English => "en",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Text {
    /// Shown after the terms, at every login.
    Announce,
    /// The billing notice of the Japanese servers; western ones only showed it.
    NoCharge,
    // Refusals (`AnsMaintenance`).
    NotConfigured,
    NotReady,
    UnsupportedDisc,
    Unpatched,
    NotAllowed,
    // The server type list (content binary 0x01): MH3SP's labels.
    ServerTypeName(usize),
    ServerTypeDescription(usize),
    /// What a city is looking for (32 slots; the last ones unnamed).
    CitySeeking(usize),
    /// A city's HR limit (8 slots).
    CityHrLimit(usize),
    /// A city's goal (64 slots; the last ones unnamed).
    CityGoal(usize),
    /// Gate `n` (1-based) of a world server.
    GateName(u16),
    /// City `n` (1-based) of a gate.
    CityName(u16),
    // Refusals shown as alerts.
    CityExists,
    CityMissing,
    CityFull,
    NoQuestSlot,
    UnknownCapcomId,
    QuestGone,
    QuestFull,
    WrongPassword,
    /// A seat (or arm wrestling place) someone else has.
    SeatTaken,
    /// A jump to another world server that can't be finished (its ticket unknown or expired).
    JumpFailed,
    /// The database failed (architecture decision 9): the request is refused, the player stays.
    Unavailable,
    /// A request the game knows that the server doesn't serve (the unknown requests, review 2026-10-11).
    NotSupported,
    /// A request repeated faster than its rate limit (quest joins).
    TooFast,
    /// A console banned (`/ban`, `access.rs`).
    Banned,
    /// A console that didn't log in through this server's Nintendo login (`nas.rs`): another server's patch, or a
    /// token expired. An alert (LMP).
    UnknownLogin,
    /// The same, refused at OPN.
    LoginElsewhere,
    /// A Wii banned (`access.rs`), refused at OPN.
    WiiBanned,
    // The town's NPCs (content binary 0x03): MH3SP's texts.
    /// Prices, normal or halved (during Jhen Mohran).
    ToolShop {
        sale: bool,
    },
    MaterialShop {
        sale: bool,
    },
    TradingPostOpen,
    /// Heads the event quest list during Jhen Mohran.
    JhenFestival,
    Guildmaster,
}

const SERVER_TYPE_NAMES: [&str; 4] = ["Open", "Rookie", "Expert", "Recruiting"];
const SERVER_TYPE_DESCRIPTIONS: [&str; 4] = [
    "Hunters of all Ranks\ncan gather here.",
    "Only hunters HR 30\nor lower may enter.",
    "Only hunters HR 31\nor higher may enter.",
    "Hunters in search of\nhunting companions\ncan gather here.",
];
const CITY_SEEKINGS: [&str; 18] = [
    "Everyone welcome!",
    "Casual play",
    "Huntin' and chattin'",
    "Beginners welcome!",
    "Skilled players only",
    "Let's Quest together",
    "Event Quest ho!",
    "Arena battles!",
    "Earning money",
    "HR grinding",
    "Gathering materials",
    "Rare material search",
    "Just a Quest or two",
    "In for the long haul",
    "Playing with friends",
    "Solo play",
    // MH3SP's own.
    "Stream Lobby",
    "Turns",
];
const CITY_HR_LIMITS: [&str; 3] = ["None", "Low", "High"];
const CITY_GOALS: [&str; 36] = [
    "None",
    "HR1~",
    "HR9~",
    "HR18~",
    "HR31~",
    "HR40~",
    "HR51~",
    "1* Urgent Quest",
    "2* Urgent Quest",
    "3* Urgent Quest",
    "4* Urgent Quest",
    "5* Urgent Quest",
    "Great Jaggi",
    "Qurupecco",
    "Royal Ludroth",
    "Barroth",
    "Gobul",
    "Rathian",
    "Great Baggi",
    "Gigginox",
    "Lagiacrus",
    "Barioth",
    "Rathalos",
    "Diablos",
    "Uragaan",
    "Agnaktor",
    "Jhen Mohran",
    "Deviljho",
    "Alatreon",
    "Mining Ore",
    "Catching Bugs",
    "Fishing",
    "Small Monsters",
    "Gathering",
    "Transporting",
    "The Arena",
];

/// `list[i]`, or a numbered placeholder past its end (`Goal40`).
fn indexed(list: &[&'static str], i: usize, placeholder: &str) -> Cow<'static, str> {
    list.get(i).map_or_else(
        || Cow::Owned(format!("{placeholder}{i}")),
        |s| Cow::Borrowed(*s),
    )
}

/// A refusal screen: the title, a line, and a highlighted line.
macro_rules! refusal {
    ($line:literal, $highlight:literal) => {
        concat!(
            "<BR><CENTER><BODY><SIZE=6>Monster Hunter 3 (Tri) Server Project<BR><BODY>",
            "<BR><CENTER><SIZE=4>",
            $line,
            "<BR><BODY><BR><CENTER><C=2>",
            $highlight,
            "<END>"
        )
    };
}

pub fn text(text: Text, _language: Language) -> Cow<'static, str> {
    Cow::Borrowed(match text {
        Text::ServerTypeName(i) => return indexed(&SERVER_TYPE_NAMES, i, "Type"),
        Text::ServerTypeDescription(i) => return indexed(&SERVER_TYPE_DESCRIPTIONS, i, "Type"),
        Text::CitySeeking(i) => return indexed(&CITY_SEEKINGS, i, "Seeking"),
        Text::CityHrLimit(i) => return indexed(&CITY_HR_LIMITS, i, "Limit"),
        Text::CityGoal(i) => return indexed(&CITY_GOALS, i, "Goal"),
        Text::GateName(n) => return Cow::Owned(format!("City Gate{n}")),
        Text::CityName(n) => return Cow::Owned(format!("City{n}")),
        Text::CityExists => "<LF=8><BODY><CENTER>City already exists.<END>",
        Text::CityMissing => "<LF=8><BODY><CENTER>This city no longer exists.<END>",
        Text::CityFull => "<LF=8><BODY><CENTER>This city is full.<END>",
        Text::NoQuestSlot => "<LF=8><BODY><CENTER>No Quest Slot Available<END>",
        Text::UnknownCapcomId => "<LF=8><BODY><CENTER>Unknown Capcom ID.<END>",
        Text::QuestGone => "<LF=8><BODY><CENTER>This quest can't be joined any more.<END>",
        // MH3SP's texts.
        Text::QuestFull => "<LF=8><BODY><CENTER>Quest is full!<END>",
        Text::WrongPassword => "<LF=8><BODY><CENTER>Wrong Password!<END>",
        Text::SeatTaken => "<LF=8><BODY><CENTER>Someone is already here.<END>",
        Text::JumpFailed => "<LF=8><BODY><CENTER>You can't go there now.<END>",
        Text::Unavailable => "<LF=8><BODY><CENTER>Not available right now.<END>",
        Text::NotSupported => "<LF=8><BODY><CENTER>Not available on this server.<END>",
        Text::TooFast => "<LF=8><BODY><CENTER>Too fast: please wait a moment.<END>",
        Text::Banned => "<LF=8><BODY><CENTER>This console is banned from this server.<END>",
        Text::UnknownLogin => {
            "<LF=8><BODY><CENTER>Log in again: your login wasn't through this server (check the patch).<END>"
        }
        Text::ToolShop { sale: true } | Text::MaterialShop { sale: true } => "Half-off sale!",
        Text::ToolShop { sale: false } | Text::MaterialShop { sale: false } => "Prices are normal.",
        Text::TradingPostOpen => "The Trading Post is open!",
        Text::JhenFestival => "The Festival of Fear",
        Text::Guildmaster => concat!(
            "To all hunters:\n",
            "Loc Lac is in \"beta,\"\n",
            "whatever that means. Also,\n",
            "the Arena is back in\n",
            "operation! And now, a:\n",
            "haiku: \"Patient warriors /\n",
            "returning home at long last /\n",
            "now join hands to hunt!\"",
        ),
        // MH3SP's texts, kept while the replay harness compares against it.
        Text::Announce => concat!(
            "<BR><CENTER><BODY><SIZE=6>Monster Hunter 3 (Tri) Server Project<BR><BODY>",
            "<BR><LEFT><SIZE=4>Welcome to the MH3SP Beta Test! We ask that you<BR><BODY>",
            "keep few things in mind while connected in order to<BR><BODY>",
            "make the experience as fun as possible for everyone.<BR><BODY>",
            "No abusing other players, no exploiting bugs, and<BR><BODY>",
            "please report bugs that you find to the server<BR><BODY>",
            "staff/dev team. We thank you for your patience as<BR><BODY>",
            "we prepared this server to be fit for public use,<BR><BODY>",
            "and ask your continued patience in the weeks to<BR><BODY>",
            "come as you may encounter some bugs and errors<BR><BODY>",
            "while playing. Please see the Public Test Server<BR><BODY>",
            "Roadmap for a list of features we are working on.<BR><BODY>",
            "<BR><CENTER><C=2>Welcome to Loc Lac!<END>",
        ),
        Text::NoCharge => "<BODY><CENTER>MH3 Server Project - No charge.<END>",
        Text::NotConfigured => refusal!(
            "This server is not configured yet.",
            "Please check back later!"
        ),
        Text::NotReady => refusal!(
            "The server is not ready.",
            "Please try again in a few minutes."
        ),
        Text::UnsupportedDisc => refusal!(
            "This version of the game is not supported.",
            "Supported: Monster Hunter Tri, PAL and US."
        ),
        Text::Unpatched => refusal!(
            "This game is not patched for this server.",
            "Please get the patch from the server's website."
        ),
        Text::NotAllowed => refusal!("This console is not allowed on this server.", ""),
        Text::LoginElsewhere => refusal!(
            "This console didn't log in through this server.",
            "Please check the patch, then try again."
        ),
        Text::WiiBanned => refusal!("This console is banned from this server.", ""),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TermsKind {
    Terms,
    SubTerms,
}

/// Terms as sent to a console: the version it remembers, and the text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Terms {
    pub version: u32,
    pub text: String,
}

/// The built-in terms are version 1; overrides (`terms` rows) start at 2.
const BUILT_IN_VERSION: u32 = 1;

/// The current terms for `language`: the latest override in that language, else the latest in English, else the
/// built-in ones.
pub async fn terms(db: &SqlitePool, kind: TermsKind, language: Language) -> sqlx::Result<Terms> {
    let (kind_name, built_in) = match kind {
        TermsKind::Terms => ("terms", "MH3 Server Project - Terms."),
        TermsKind::SubTerms => ("subterms", "MH3 Server Project - SubTerms."),
    };
    let code = language.code();
    let row = sqlx::query!(
        r#"SELECT id AS "id!: u32", value FROM terms
           WHERE kind = ?1 AND language IN (?2, 'en')
           ORDER BY language = ?2 DESC, id DESC LIMIT 1"#,
        kind_name,
        code
    )
    .fetch_optional(db)
    .await?;
    Ok(match row {
        Some(row) => Terms {
            version: row.id,
            text: row.value,
        },
        None => Terms {
            version: BUILT_IN_VERSION,
            text: built_in.to_owned(),
        },
    })
}
