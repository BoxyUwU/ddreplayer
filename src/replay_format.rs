use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct ReplayInfo {
    raw_name: String,
    pretty_name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RawReplayInfo {
    pretty_name: String,
}

#[test]
fn test() {
    let x: RawReplayInfo = ron::from_str(r#"(pretty_name: "Foo")"#).unwrap();
    dbg!(x);
}
