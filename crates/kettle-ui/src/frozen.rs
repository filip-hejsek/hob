//! [`FrozenIsland`]: an island whose fields and state can't change.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::widget::Widget;
use crate::{Contents, Error, Fields, Image, Value, collect_fields};

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

    /// The images in named state.
    pub fn images(&self) -> impl Iterator<Item = &Image> {
        self.state.values().filter_map(|v| match v {
            Value::Image(image) => Some(image),
            _ => None,
        })
    }

    /// A copy with each image in named state replaced by `f`'s result, e.g.
    /// to save images separately (see [`Image::without_data`]).
    pub fn map_images(&self, mut f: impl FnMut(&Image) -> Image) -> FrozenIsland {
        let mut island = self.clone();
        for value in island.state.values_mut() {
            if let Value::Image(image) = value {
                *image = f(image);
            }
        }
        island
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
            row([
                submit_button("OK"),
                button("del", "Delete"),
                button_danger("rm", "Remove"),
            ]),
        ]));
        b.set_text("name", "Ada");
        b.set_checked("sub", true);
        b.derive("hi", |i| format!("Hi, {}!", i.text("name")).into());
        b.set_state("items", vec!["a".to_owned(), "b".to_owned()]);
        b.set_state("p", 0.5);
        b.set_state("flag", true);
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
        assert_eq!(back.state("flag"), Some(&Value::Bool(true)));
        assert_eq!(json, serde_json::to_string(&back).unwrap());
    }

    #[test]
    fn values_are_plain_json() {
        let json = serde_json::to_value(frozen()).unwrap();
        assert_eq!(json["fields"], json!({"name": "Ada", "sub": true}));
        assert_eq!(json["state"]["items"], json!(["a", "b"]));
        assert_eq!(json["state"]["flag"], json!(true));
        assert_eq!(
            json["root"]["column"][0],
            json!({"text_field": {"id": "name", "placeholder": "Name"}})
        );
        assert_eq!(
            json["root"]["column"][5]["row"][1],
            json!({"button": {"label": "Delete", "action": {"press": "del"}}})
        );
        assert_eq!(
            json["root"]["column"][5]["row"][2],
            json!({"button": {"label": "Remove", "action": {"press": "rm"}, "danger": true}})
        );
    }

    #[test]
    fn images_are_objects() {
        let dot = Image::from_rgba(1, 1, &[1, 2, 3, 255], "dot").unwrap();
        let mut b = IslandBuilder::new(image("img"));
        b.set_state("img", dot.clone());
        let mut json = serde_json::to_value(b.freeze().unwrap()).unwrap();
        let img = &json["state"]["img"];
        assert_eq!(img["key"], json!(dot.key().to_string()));
        assert_eq!(img["alt"], json!("dot"));
        assert_eq!((&img["width"], &img["height"]), (&json!(1), &json!(1)));
        assert!(img["format"].is_string() && img["data"].is_string());
        assert!(img.get("path").is_none());

        let back: FrozenIsland = serde_json::from_value(json.clone()).unwrap();
        let Some(Value::Image(back)) = back.state("img") else {
            panic!("not an image");
        };
        assert_eq!(back, &dot);
        assert_eq!(back.data(), dot.data());

        // Without the data, it's an image without data.
        let img = json["state"]["img"].as_object_mut().unwrap();
        img.retain(|k, _| k == "key" || k == "alt");
        let back: FrozenIsland = serde_json::from_value(json).unwrap();
        assert_eq!(back.state("img"), Some(&Value::Image(dot.without_data())));
        assert_eq!(back.images().count(), 1);
        assert_eq!(Value::Image(dot).to_string(), "dot");
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
    fn values_deserialize_from_plain_json() {
        let value = |v| serde_json::from_value::<Value>(v).unwrap();
        assert_eq!(value(json!("hi")), Value::from("hi"));
        assert_eq!(value(json!(2)), Value::Number(2.0));
        assert_eq!(value(json!(["a"])), Value::List(vec!["a".into()]));
        assert_eq!(value(json!(false)), Value::Bool(false));
        assert!(matches!(value(json!(null)), Value::Number(n) if n.is_nan()));
        assert_eq!(Value::Bool(true).to_string(), "true");
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
