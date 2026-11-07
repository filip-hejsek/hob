//! Drives the demo islands headlessly.

use std::time::Duration;

use kettle_ui::{Outcome, Value};
use kettle_ui_demo::Demo;
use kettle_ui_headless::Island;

#[test]
fn contact() {
    let mut island = Island::new(Demo::Contact.builder()).unwrap();
    island.type_text("name", "Ada").unwrap();
    island.set_checked("subscribe", true).unwrap();
    assert_eq!(
        island.to_string(),
        r#"column
  label "Name"
  text_field name: "Ada"
  label "Hello, Ada!"
  checkbox subscribe: [x] "Subscribe to updates"
  row
    button "Submit"
    button "Cancel"
"#
    );

    let Outcome::Submitted(fields) = island.press("Submit").unwrap() else {
        panic!("expected a submission");
    };
    assert_eq!(
        Demo::Contact.summary(&fields),
        r#"name "Ada", subscribe true"#
    );
    assert!(matches!(island.press("Cancel"), Ok(Outcome::Cancelled)));
}

#[test]
fn fruit_search() {
    let mut island = Island::new(Demo::FruitSearch.builder()).unwrap();
    island.type_text("fruit", "berry").unwrap();
    island.pick("fruit", "blueberry").unwrap();
    assert_eq!(island.fields().text("fruit"), "blueberry");

    island.type_text("fruit", "xyz").unwrap();
    assert_eq!(island.state("matches"), Some(&Value::List(Vec::new())));
    assert_eq!(island.state("no_matches"), Some(&Value::Bool(true)));
    assert!(island.to_string().contains("label \"No matching fruit\""));

    island.type_text("fruit", "ap").unwrap();
    // Wait for the pretend loading to finish, so the snapshot is stable.
    let loaded = |i: &Island| i.state("status") == Some(&Value::from("Fruit loaded"));
    assert!(island.wait_until(Duration::from_secs(30), loaded));
    assert_eq!(
        island.to_string(),
        r#"column
  label "Fruit loaded"
  progress 100%
  label "Favourite fruit"
  text_field fruit: "ap"
  suggestions for fruit
    - "apple"
    - "apricot"
    - "grape"
    - "grapefruit"
    - "pineapple"
  row
    button "Choose"
    button "Cancel"
"#
    );
}
