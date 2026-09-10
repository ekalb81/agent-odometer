//! Generated browser prices must remain responses from the production service.
//! Frontend tests separately bind these raw inputs to the actual fixture factory.
use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use odometer_lib::model::{Session, SessionSummary};
use odometer_lib::query::{price_session_details, price_summary, price_surfaces};
use odometer_lib::rates::RateCard;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
struct FixtureFile {
    schema_version: u32,
    input: Input,
    cases: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
struct Input {
    now: DateTime<Utc>,
    rates: RateCard,
    cases: BTreeMap<String, Case>,
}

#[derive(Deserialize)]
struct Case {
    summary: SessionSummary,
    detail: Session,
}

fn normalize_json_numbers(value: &mut Value) {
    match value {
        Value::Number(number) => {
            // JavaScript serializes integral prices as `0`, Rust as `0.0`.
            *number = serde_json::Number::from_f64(number.as_f64().unwrap()).unwrap();
        }
        Value::Array(items) => items.iter_mut().for_each(normalize_json_numbers),
        Value::Object(entries) => entries.values_mut().for_each(normalize_json_numbers),
        _ => {}
    }
}

#[test]
fn browser_fixture_prices_are_current_production_responses() {
    let fixtures: FixtureFile =
        serde_json::from_str(include_str!("../../src/dev-mock/pricing.generated.json")).unwrap();
    assert_eq!(fixtures.schema_version, 1);
    assert_eq!(fixtures.cases.len(), fixtures.input.cases.len());
    assert!(!fixtures.cases.is_empty());
    let Input { now, rates, cases } = fixtures.input;
    for (key, case) in cases {
        let actual = json!({
            "summary": price_summary(&case.summary, &rates, now),
            "range": price_surfaces(&case.summary.buckets, case.summary.harness.as_str(), &rates, now),
            "detail": price_session_details(case.detail, &rates, now).pricing,
        });
        // Compare both through the same JSON read path: serde_json's default
        // float reader can round a serialized decimal by one binary ULP.
        let mut actual: Value =
            serde_json::from_str(&serde_json::to_string(&actual).unwrap()).unwrap();
        let mut expected = fixtures.cases[&key].clone();
        normalize_json_numbers(&mut actual);
        normalize_json_numbers(&mut expected);
        assert_eq!(actual, expected, "browser fixture drift: {key}");
    }
}
