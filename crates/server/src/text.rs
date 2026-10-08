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
