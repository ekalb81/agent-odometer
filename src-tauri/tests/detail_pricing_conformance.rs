//! Shared immutable detail oracle captured from the pre-migration desktop
//! engine. Checks production DTO serialization, not a second pricing formula.
use odometer_lib::model::{Session, SessionSummary};
use odometer_lib::query::{price_session_details, price_summary};
use odometer_lib::rates::RateCard;
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/conformance/detail-pricing-cases.json"
    ))
    .unwrap()
}

fn merge(base: &Value, overlay: &Value) -> Value {
    let mut result = base.clone();
    if let Some(fields) = overlay.as_object() {
        result.as_object_mut().unwrap().extend(fields.clone());
    }
    result
}

fn normalize(value: &mut Value) {
    match value {
        Value::Number(number) => {
            let rounded = (number.as_f64().unwrap() * 1e9).round() / 1e9;
            *number = serde_json::Number::from_f64(rounded).unwrap();
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                normalize(item);
            }
            // DTO arrays are sets or keyed breakdowns; their insertion order
            // is representational, not a pricing difference.
            items.sort_by_key(|item| {
                item.get("model")
                    .or_else(|| item.get("id"))
                    .unwrap_or(item)
                    .to_string()
            });
        }
        Value::Object(fields) => {
            for field in fields.values_mut() {
                normalize(field);
            }
        }
        _ => {}
    }
}

#[test]
fn serialized_detail_pricing_matches_legacy_desktop_for_every_case() {
    let fixture = fixture();
    let now = fixture["now"].as_str().unwrap().parse().unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 16, "corpus must not silently lose cases");
    for case in cases {
        let session: Session =
            serde_json::from_value(merge(&fixture["base_session"], &case["session"]))
                .unwrap_or_else(|error| panic!("{}: session {error}", case["name"]));
        let rates: RateCard =
            serde_json::from_value(merge(&fixture["rate_card"], &case["rate_overrides"]))
                .unwrap_or_else(|error| panic!("{}: rates {error}", case["name"]));
        let original = serde_json::to_value(&session).unwrap();
        let details = price_session_details(session, &rates, now);
        let mut wire = serde_json::to_value(details).unwrap();
        let mut actual = wire.as_object_mut().unwrap().remove("pricing").unwrap();
        for field in [
            "current",
            "dated_purchased_credits",
            "dated_included_allowance",
        ] {
            actual.as_object_mut().unwrap().remove(field);
        }
        for prices in actual["turn_prices"].as_object_mut().unwrap().values_mut() {
            prices.as_object_mut().unwrap().remove("current");
        }
        // Price attachment must not alter raw details or persist derived data.
        assert_eq!(
            wire, original,
            "{}: flattened session changed",
            case["name"]
        );
        // Retain the captured desktop oracle while naming intentional
        // correctness fixes; overrides are explicit, never regenerated.
        if case.get("backend_expected_overrides").is_some() {
            assert!(case["divergence_reason"]
                .as_str()
                .is_some_and(|reason| !reason.is_empty()));
        }
        let mut expected = merge(&case["expected"], &case["backend_expected_overrides"]);
        // #248: fallback model identity does not establish speed support.
        // Keep the captured oracle immutable and state the intentional change.
        if case["name"] == "fast session and turn costs preserve tier multiplier" {
            expected["flat_api"] = serde_json::json!({ "total": 0, "by_model": [{"model":"gpt-5.5","cost":0,"basis":"unavailable","unpriced":true}], "missing_models":["gpt-5.5"],"unpriced_models":["gpt-5.5"] });
            expected["turn_prices"]["fast"]["api"] = serde_json::json!({"cost":0,"fallback_used":true,"unpriced":true,"basis":"unavailable"});
        }
        // #248: an absent legacy rate is unavailable in the model row and total.
        if case["name"] == "no history unavailable model retains zero breakdown" {
            expected["plan"]["by_model"][0]["unpriced"] = serde_json::json!(true);
            expected["plan"]["unpriced_models"] = serde_json::json!(["unknown"]);
        }
        normalize(&mut actual);
        normalize(&mut expected);
        assert_eq!(actual, expected, "{}", case["name"]);
    }
}

#[test]
fn no_history_summary_stays_bucket_priced_while_details_retain_legacy_api_zero() {
    let fixture = fixture();
    let session: Session = serde_json::from_value(fixture["base_session"].clone()).unwrap();
    let rates: RateCard = serde_json::from_value(fixture["rate_card"].clone()).unwrap();
    let now = fixture["now"].as_str().unwrap().parse().unwrap();
    let summary = price_summary(&SessionSummary::of(&session), &rates, now);
    let details = price_session_details(session, &rates, now);
    assert_eq!(summary.pricing.plan, details.pricing.plan);
    assert!(summary.pricing.api.unwrap().total > 0.0);
    assert_eq!(details.pricing.flat_api.unwrap().total, 0.0);
    assert!(details.pricing.time_aware_api.is_none());
}

#[test]
fn summary_categories_reuse_the_shared_plan_and_api_surface_contract() {
    let fixture = fixture();
    let session: Session = serde_json::from_value(fixture["base_session"].clone()).unwrap();
    let rates: RateCard = serde_json::from_value(fixture["rate_card"].clone()).unwrap();
    let now = fixture["now"].as_str().unwrap().parse().unwrap();
    let mut summary = SessionSummary::of(&session);
    summary.category_totals = serde_json::from_value(serde_json::json!({
        "other": {
            "turns": 1,
            "tokens": summary.tokens_total,
            "tool_calls": 0,
            "buckets": summary.buckets,
        }
    }))
    .unwrap();
    let priced = price_summary(&summary, &rates, now);
    assert_eq!(priced.categories["other"], priced.pricing);
    let mut actual = serde_json::to_value(&priced.categories["other"].plan).unwrap();
    let mut expected = fixture["cases"][0]["expected"]["plan"].clone();
    normalize(&mut actual);
    normalize(&mut expected);
    assert_eq!(actual, expected);
}
