//! Capture and replay of `SQLite` functions whose results vary between runs.
//!
//! Selected built-ins are overridden on each replicated writer connection.
//! During local execution, the override calls the original built-in on a
//! separate in-memory connection and records its result. During replay, it
//! returns the recorded result without evaluating the function again.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, MutexGuard},
};

use rusqlite::{
    Connection,
    functions::{Context, FunctionFlags},
    params_from_iter,
    types::Value,
};

use crate::{
    error::Error,
    protocol::{CapturedFunction, CapturedFunctionCall, CapturedFunctionCalls},
};

/// Describes one `SQLite` built-in intercepted by the replicated connection.
#[derive(Clone, Copy)]
struct FunctionDefinition {
    function: CapturedFunction,
    name: &'static str,
    argument_count: i32,
    keyword: bool,
}

// SQLite functions whose results are captured during local execution and
// supplied in the same order during replay on other replicas.
const FUNCTIONS: [FunctionDefinition; 12] = [
    FunctionDefinition::new(CapturedFunction::Random, "random", 0),
    FunctionDefinition::new(CapturedFunction::RandomBlob, "randomblob", 1),
    FunctionDefinition::variadic(CapturedFunction::Date, "date"),
    FunctionDefinition::variadic(CapturedFunction::Time, "time"),
    FunctionDefinition::variadic(CapturedFunction::DateTime, "datetime"),
    FunctionDefinition::variadic(CapturedFunction::JulianDay, "julianday"),
    FunctionDefinition::variadic(CapturedFunction::UnixEpoch, "unixepoch"),
    FunctionDefinition::variadic(CapturedFunction::Strftime, "strftime"),
    FunctionDefinition::new(CapturedFunction::TimeDiff, "timediff", 2),
    FunctionDefinition::keyword(CapturedFunction::CurrentDate, "current_date"),
    FunctionDefinition::keyword(CapturedFunction::CurrentTime, "current_time"),
    FunctionDefinition::keyword(CapturedFunction::CurrentTimestamp, "current_timestamp"),
];

impl FunctionDefinition {
    const fn new(function: CapturedFunction, name: &'static str, argument_count: i32) -> Self {
        Self {
            function,
            name,
            argument_count,
            keyword: false,
        }
    }

    const fn variadic(function: CapturedFunction, name: &'static str) -> Self {
        Self::new(function, name, -1)
    }

    const fn keyword(function: CapturedFunction, name: &'static str) -> Self {
        Self {
            function,
            name,
            argument_count: 0,
            keyword: true,
        }
    }

    fn query(self, argument_count: usize) -> String {
        if self.keyword {
            return format!("SELECT {}", self.name);
        }

        let parameters = std::iter::repeat_n("?", argument_count)
            .collect::<Vec<_>>()
            .join(", ");

        format!("SELECT {}({parameters})", self.name)
    }
}

/// Behavior of the installed callbacks for the current writer operation.
/// Only one capture or replay session can be active on a connection.
enum Mode {
    Passthrough,
    Capture(Vec<CapturedFunctionCall>),
    Replay {
        calls: VecDeque<CapturedFunctionCall>,
        failed: bool,
    },
}

/// Controls the function implementations installed on one replicated writer.
pub struct FunctionOverrides {
    state: Arc<Mutex<Mode>>,
}

impl FunctionOverrides {
    /// Installs the overrides and creates the untouched `SQLite` connection
    /// used to evaluate the original built-ins during capture.
    pub fn install(connection: &Connection) -> Result<Self, Error> {
        let state = Arc::new(Mutex::new(Mode::Passthrough));
        let evaluator = Arc::new(Mutex::new(Connection::open_in_memory()?));

        for definition in FUNCTIONS {
            let state = Arc::clone(&state);
            let evaluator = Arc::clone(&evaluator);

            connection.create_scalar_function(
                definition.name,
                definition.argument_count,
                FunctionFlags::SQLITE_UTF8,
                move |context| invoke(definition, context, &state, &evaluator),
            )?;
        }

        Ok(Self { state })
    }

    /// Starts recording function calls made by one local transaction.
    pub fn capture(&mut self) -> CaptureSession<'_> {
        *lock(&self.state) = Mode::Capture(Vec::new());

        CaptureSession::new(self)
    }

    /// Starts replaying the recorded calls for one received transaction.
    pub fn replay(&mut self, calls: &CapturedFunctionCalls) -> ReplaySession<'_> {
        *lock(&self.state) = Mode::Replay {
            calls: calls.as_slice().iter().cloned().collect(),
            failed: false,
        };

        ReplaySession::new(self)
    }
}

/// Restores passthrough mode if capture exits before [`Self::finish`].
pub struct CaptureSession<'a> {
    overrides: &'a mut FunctionOverrides,
    active: bool,
}

impl<'a> CaptureSession<'a> {
    const fn new(overrides: &'a mut FunctionOverrides) -> Self {
        Self {
            overrides,
            active: true,
        }
    }

    pub fn finish(mut self) -> Result<CapturedFunctionCalls, Error> {
        let mode = take_mode(&self.overrides.state);
        self.active = false;

        let Mode::Capture(calls) = mode else {
            unreachable!("capture guard must own an active capture session");
        };

        CapturedFunctionCalls::new(calls)
    }
}

impl Drop for CaptureSession<'_> {
    fn drop(&mut self) {
        if self.active {
            reset(&self.overrides.state);
        }
    }
}

/// Restores passthrough mode if replay exits before [`Self::finish`].
pub struct ReplaySession<'a> {
    overrides: &'a mut FunctionOverrides,
    active: bool,
}

impl<'a> ReplaySession<'a> {
    const fn new(overrides: &'a mut FunctionOverrides) -> Self {
        Self {
            overrides,
            active: true,
        }
    }

    pub fn failed(&self) -> bool {
        matches!(
            &*lock(&self.overrides.state),
            Mode::Replay { failed: true, .. }
        )
    }

    pub fn finish(mut self) -> Result<(), Error> {
        let mode = take_mode(&self.overrides.state);
        self.active = false;

        let Mode::Replay { calls, failed } = mode else {
            unreachable!("replay guard must own an active replay session");
        };

        if failed {
            return Err(Error::InvalidPayload(
                "captured SQLite function call does not match replay",
            ));
        }

        if !calls.is_empty() {
            return Err(Error::InvalidPayload(
                "captured SQLite function results were not consumed",
            ));
        }

        Ok(())
    }
}

impl Drop for ReplaySession<'_> {
    fn drop(&mut self) {
        if self.active {
            reset(&self.overrides.state);
        }
    }
}

fn invoke(
    definition: FunctionDefinition,
    context: &Context<'_>,
    state: &Mutex<Mode>,
    evaluator: &Mutex<Connection>,
) -> rusqlite::Result<Value> {
    let mut mode = lock(state);

    match &mut *mode {
        Mode::Passthrough => evaluate(definition, context, evaluator),
        Mode::Capture(calls) => {
            let result = evaluate(definition, context, evaluator)?;
            let call = CapturedFunctionCall::new(definition.function, result.clone())
                .map_err(|error| rusqlite::Error::UserFunctionError(Box::new(error)))?;

            calls.push(call);

            Ok(result)
        }
        Mode::Replay { calls, failed } => {
            let Some(call) = calls.pop_front() else {
                *failed = true;
                return Err(replay_error("captured SQLite function result is missing"));
            };

            if call.function != definition.function {
                *failed = true;
                return Err(replay_error(
                    "captured SQLite function order does not match",
                ));
            }

            Ok(call.result.into_value())
        }
    }
}

fn evaluate(
    definition: FunctionDefinition,
    context: &Context<'_>,
    evaluator: &Mutex<Connection>,
) -> rusqlite::Result<Value> {
    // Calling the function on the replicated connection would recurse into
    // this override. The evaluator connection still has SQLite's built-in.
    let arguments = (0..context.len())
        .map(|index| context.get_raw(index).into())
        .collect::<Vec<Value>>();
    let query = definition.query(arguments.len());

    lock(evaluator).query_row(&query, params_from_iter(arguments), |row| row.get(0))
}

fn replay_error(message: &'static str) -> rusqlite::Error {
    rusqlite::Error::UserFunctionError(message.into())
}

fn take_mode(state: &Mutex<Mode>) -> Mode {
    std::mem::replace(&mut *lock(state), Mode::Passthrough)
}

fn reset(state: &Mutex<Mode>) {
    *lock(state) = Mode::Passthrough;
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
