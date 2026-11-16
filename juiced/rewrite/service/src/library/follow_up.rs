use super::{Library, PullRecord};
use crate::config::Policy;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// What the console asked for when it started a pull: register the files, or move a model to
/// them. It is journaled with the pull, so the intent outlives a page reload or a restart.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FollowUp {
    Add {
        #[serde(default)]
        id: Option<String>,
        policy: Policy,
        #[serde(default)]
        engine: Option<String>,
    },
    Update {
        model: String,
        from_artifact: String,
    },
}

/// The public request shape; updates start only through the model update route.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddWhenDone {
    #[serde(default)]
    pub id: Option<String>,
    pub policy: Policy,
    #[serde(default)]
    pub engine: Option<String>,
}

impl From<AddWhenDone> for FollowUp {
    fn from(value: AddWhenDone) -> Self {
        Self::Add {
            id: value.id,
            policy: value.policy,
            engine: value.engine,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Outcome {
    pub state: OutcomeState,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeState {
    #[default]
    Pending,
    Done,
    Failed,
}

impl Outcome {
    pub fn done(model: &str) -> Self {
        Self {
            state: OutcomeState::Done,
            model: Some(model.to_owned()),
            ..Self::default()
        }
    }
    pub fn failed(code: &str, message: impl Into<String>) -> Self {
        Self {
            state: OutcomeState::Failed,
            error: Some(code.to_owned()),
            message: Some(message.into()),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Saved {
    pub then: FollowUp,
    #[serde(default)]
    pub outcome: Outcome,
}

pub(super) fn view(follow_up: Option<&Saved>) -> Value {
    let Some(Saved { then, outcome }) = follow_up else {
        return Value::Null;
    };
    let mut view = serde_json::to_value(then).unwrap_or(Value::Null);
    view["state"] = json!(outcome.state);
    view["error"] = json!(outcome.error);
    view["message"] = json!(outcome.message);
    if let Some(model) = &outcome.model {
        view["model"] = json!(model);
    }
    view
}

impl PullRecord {
    pub(super) fn follow_up_pending(&self) -> bool {
        self.follow_up
            .as_ref()
            .is_some_and(|saved| saved.outcome.state == OutcomeState::Pending)
    }
}

impl Library {
    /// Pull ids whose follow-up still waits, for the supervisor to resume after a restart.
    pub(crate) fn pending_follow_ups(&self) -> Vec<String> {
        self.pulls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter_map(|record| {
                let record = record.lock().unwrap_or_else(|error| error.into_inner());
                record.follow_up_pending().then(|| record.id.clone())
            })
            .collect()
    }

    pub(crate) fn follow_up(&self, pull: &str) -> Option<FollowUp> {
        self.with_pull(pull, |record| {
            record.follow_up.as_ref().map(|saved| saved.then.clone())
        })
        .flatten()
    }

    /// Only one task drives a pull's follow-up; later callers get false.
    pub(crate) fn claim_follow_up(&self, pull: &str) -> bool {
        self.following
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(pull.to_owned())
    }

    /// Gives up the claim without an outcome, so a later recovery resumes the follow-up.
    pub(crate) fn release_follow_up(&self, pull: &str) {
        self.following
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(pull);
    }

    pub(crate) fn settle_follow_up(&self, pull: &str, outcome: Outcome) {
        self.release_follow_up(pull);
        self.with_pull(pull, |record| {
            if let Some(saved) = &mut record.follow_up {
                saved.outcome = outcome;
            }
            self.persist(record);
        });
    }

    fn with_pull<T>(&self, pull: &str, action: impl FnOnce(&mut PullRecord) -> T) -> Option<T> {
        let record = self
            .pulls
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .find(|record| record.lock().unwrap_or_else(|error| error.into_inner()).id == pull)
            .cloned()?;
        let mut record = record.lock().unwrap_or_else(|error| error.into_inner());
        Some(action(&mut record))
    }
}
