//! Logos SQL Cucumber bindings, split into step entrypoints and the behavior
//! they exercise.

mod actions;
mod assertions;
mod steps;
mod tables;

use std::str::FromStr;

use cucumber::Parameter;

#[derive(Clone, Copy, Debug, Parameter)]
#[param(name = "logos_sql_database", regex = "live|finalized")]
enum DatabaseKind {
    Live,
    Finalized,
}

impl FromStr for DatabaseKind {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "live" => Ok(Self::Live),
            "finalized" => Ok(Self::Finalized),
            value => Err(format!("unknown Logos SQL database '{value}'")),
        }
    }
}

impl DatabaseKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Finalized => "finalized",
        }
    }
}
