//! The people reads' commands: `people` finds people as the search's People tab does, `person` shows one person's
//! profile card, and `presence` reads the presence of people as the presence badges do.

use clap::Subcommand;

use crate::protocol::Request;

#[derive(Debug, Subcommand)]
pub enum PeopleCommand {
    /// Find people by name, email or UPN, as the search's People tab finds them.
    People {
        /// The words searched for.
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
    },
    /// One person's profile card, with their presence: by MRI, UPN, email, or a name only one person has.
    Person {
        #[arg(value_name = "MRI|UPN|NAME")]
        who: String,
    },
    /// The presence of people, each by MRI, UPN, email, or a name only one person has (20 at most).
    Presence {
        #[arg(required = true, num_args = 1..=crate::protocol::PRESENCE_MOST, value_name = "MRI|UPN|NAME")]
        people: Vec<String>,
    },
}

impl PeopleCommand {
    /// The request the command asks.
    #[must_use]
    pub fn request(&self) -> Request {
        match self {
            Self::People { query, limit } => Request::People {
                query: query.join(" "),
                limit: *limit,
            },
            Self::Person { who } => Request::Person { who: who.clone() },
            Self::Presence { people } => Request::Presence { people: people.clone() },
        }
    }
}
