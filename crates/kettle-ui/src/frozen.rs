//! [`FrozenIsland`]: an island whose fields and state can't change.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::widget::Widget;
use crate::{Contents, Fields, Value};

/// An island whose fields and named state can't change, e.g. to show what the
/// user submitted. Backends show it with all interaction disabled.
///
/// Create one with [`IslandBuilder::freeze`](crate::IslandBuilder::freeze),
/// or take a snapshot of a live island with [`Model::freeze`](crate::Model::freeze).
/// Cloning is cheap for the widget tree, which is shared.
#[derive(Clone)]
pub struct FrozenIsland {
    root: Arc<Widget>,
    fields: Fields,
    state: HashMap<String, Value>,
    outdated: HashSet<String>,
}

impl FrozenIsland {
    pub(crate) fn new(
        root: Arc<Widget>,
        fields: Fields,
        state: HashMap<String, Value>,
        outdated: HashSet<String>,
    ) -> Self {
        FrozenIsland {
            root,
            fields,
            state,
            outdated,
        }
    }

    /// The widget tree.
    pub fn root(&self) -> &Widget {
        &self.root
    }

    /// The values of all form elements.
    pub fn fields(&self) -> &Fields {
        &self.fields
    }

    /// The value of named state; `None` if it wasn't computed when the island
    /// was frozen.
    pub fn state(&self, name: &str) -> Option<&Value> {
        self.state.get(name)
    }

    /// Whether named state was outdated when the island was frozen (see
    /// [`Model::is_outdated`](crate::Model::is_outdated)).
    pub fn is_outdated(&self, name: &str) -> bool {
        self.outdated.contains(name)
    }
}

impl Contents for FrozenIsland {
    fn root(&self) -> &Widget {
        self.root()
    }

    fn fields(&self) -> &Fields {
        self.fields()
    }

    fn state(&self, name: &str) -> Option<&Value> {
        self.state(name)
    }

    fn is_outdated(&self, name: &str) -> bool {
        self.is_outdated(name)
    }
}
