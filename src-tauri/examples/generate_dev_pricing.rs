//! Offline synthetic browser fixtures. Never reads the user's ledger or rates.
use std::collections::BTreeMap;
use std::io::{self, Read};

use chrono::{DateTime, Utc};
use odometer_lib::model::{Session, SessionSummary};
use odometer_lib::query::{price_session_details, price_summary, price_surfaces};
use odometer_lib::rates::RateCard;
use serde::Deserialize;

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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;
    let input: Input = serde_json::from_str(&input)?;
    let cases: BTreeMap<_, _> = input
        .cases
        .into_iter()
        .map(|(key, case)| {
            let summary = price_summary(&case.summary, &input.rates, input.now);
            let range = price_surfaces(
                &case.summary.buckets,
                case.summary.harness.as_str(),
                &input.rates,
                input.now,
            );
            let detail = price_session_details(case.detail, &input.rates, input.now).pricing;
            (
                key,
                serde_json::json!({ "summary": summary, "range": range, "detail": detail }),
            )
        })
        .collect();
    serde_json::to_writer(io::stdout().lock(), &cases)?;
    Ok(())
}
