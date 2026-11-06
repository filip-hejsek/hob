//! [`FrozenIsland`]: an island whose fields and state can't change.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::widget::Widget;
use crate::{Contents, Error, Fields, Value, collect_fields};

/// An island whose fields and named state can't change, e.g. to show what the
/// user submitted. Backends show it with all interaction disabled.
///
/// Create one with [`IslandBuilder::freeze`](crate::IslandBuilder::freeze),
/// or take a snapshot of a live island with [`Model::freeze`](crate::Model::freeze).
/// Cloning is cheap for the widget tree, which is shared.
///
/// It can be serialized with serde. Deserializing checks that the field
/// values fit the form elements in the tree.
#[derive(Clone, Serialize, Deserialize)]
#[serde(try_from = "Unchecked")]
pub struct FrozenIsland {
    root: Arc<Widget>,
    fields: Fields,
    state: BTreeMap<String, Value>,
    outdated: BTreeSet<String>,
}

impl FrozenIsland {
    pub(crate) fn new(
        root: Arc<Widget>,
        fields: Fields,
        state: BTreeMap<String, Value>,
        outdated: BTreeSet<String>,
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

/// A deserialized [`FrozenIsland`], before it's checked.
#[derive(Deserialize)]
struct Unchecked {
    root: Arc<Widget>,
    fields: Fields,
    state: BTreeMap<String, Value>,
    outdated: BTreeSet<String>,
}

impl TryFrom<Unchecked> for FrozenIsland {
    type Error = Error;

    fn try_from(island: Unchecked) -> Result<Self, Error> {
        let mut fields = Fields::default();
        collect_fields(&island.root, &mut fields)?;
        for (id, value) in island.fields.0 {
            fields.set(&id, value)?;
        }
        Ok(FrozenIsland::new(
            island.root,
            fields,
            island.state,
            island.outdated,
        ))
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::IslandBuilder;
    use crate::widget::*;

    fn frozen() -> FrozenIsland {
        let mut b = IslandBuilder::new(column([
            text_field("name", "Name"),
            checkbox("sub", "Subscribe"),
            label(state("hi")),
            list("items"),
            progress("p"),
            submit_button("OK"),
        ]));
        b.set_text("name", "Ada");
        b.set_checked("sub", true);
        b.derive("hi", |i| format!("Hi, {}!", i.text("name")).into());
        b.set_state("items", vec!["a".to_owned(), "b".to_owned()]);
        b.set_state("p", 0.5);
        b.freeze().unwrap()
    }

    #[test]
    fn round_trips_through_json() {
        let island = frozen();
        let json = serde_json::to_string(&island).unwrap();
        let back: FrozenIsland = serde_json::from_str(&json).unwrap();
        assert_eq!(back.fields().text("name"), "Ada");
        assert!(back.fields().checked("sub"));
        assert_eq!(back.state("hi"), Some(&"Hi, Ada!".into()));
        assert_eq!(back.state("p"), Some(&0.5.into()));
        assert_eq!(json, serde_json::to_string(&back).unwrap());
    }

    #[test]
    fn values_are_plain_json() {
        let json = serde_json::to_value(frozen()).unwrap();
        assert_eq!(json["fields"], json!({"name": "Ada", "sub": true}));
        assert_eq!(json["state"]["items"], json!(["a", "b"]));
        assert_eq!(
            json["root"]["column"][0],
            json!({"text_field": {"id": "name", "placeholder": "Name"}})
        );
    }

    #[test]
    fn non_finite_numbers_are_restored_as_nan() {
        let mut b = IslandBuilder::new(column([progress("p"), progress("q")]));
        b.set_state("p", f64::NAN);
        b.set_state("q", f64::INFINITY);
        let json = serde_json::to_string(&b.freeze().unwrap()).unwrap();
        let back: FrozenIsland = serde_json::from_str(&json).unwrap();
        for name in ["p", "q"] {
            assert!(matches!(back.state(name), Some(Value::Number(n)) if n.is_nan()));
        }
    }

    #[test]
    fn deserializing_checks_the_fields() {
        let mut json = serde_json::to_value(frozen()).unwrap();
        json["fields"]["sub"] = json!("text");
        let error = serde_json::from_value::<FrozenIsland>(json).err().unwrap();
        assert!(error.to_string().contains("not a text field"), "{error}");

        let mut json = serde_json::to_value(frozen()).unwrap();
        json["fields"]["nope"] = json!(true);
        assert!(serde_json::from_value::<FrozenIsland>(json).is_err());

        // Missing fields get their default value.
        let mut json = serde_json::to_value(frozen()).unwrap();
        json["fields"] = json!({});
        let island: FrozenIsland = serde_json::from_value(json).unwrap();
        assert_eq!(island.fields().text("name"), "");
    }
}
