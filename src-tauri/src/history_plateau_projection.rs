//! Project a proven retained Codex repair into its existing snapshot and summary.
//! All session data stays inside the migration transaction in SQLite.
use anyhow::Result;
use rusqlite::Transaction;

const DIMENSIONS: [&str; 6] = [
    "input_tokens",
    "cached_input_tokens",
    "cache_creation_input_tokens",
    "output_tokens",
    "reasoning_output_tokens",
    "total_tokens",
];

fn vector(value: impl Fn(&str) -> String) -> String {
    let fields = DIMENSIONS.map(|field| format!("'{field}', {}", value(field)));
    format!("json_object({})", fields.join(", "))
}

fn sum_vector(column: &str) -> String {
    vector(|field| format!("coalesce(sum(json_extract({column}, '$.{field}')), 0)"))
}

fn matches_vector(object: &str, expected: impl Fn(&str) -> String) -> String {
    DIMENSIONS
        .map(|field| {
            let path = format!("'$.{field}'");
            let actual = format!("json_extract({object}, {path})");
            let comparison = format!(
                "(json_type({object}, {path}) IS 'integer' AND {actual} IS {})",
                expected(field)
            );
            if field == "cache_creation_input_tokens" {
                // This dimension was absent from older serialized TokenTotals (Serde default zero).
                format!(
                    "({comparison} OR (json_type({object}, {path}) IS NULL AND {} IS 0))",
                    expected(field)
                )
            } else {
                comparison
            }
        })
        .join(" AND ")
}

// Chrono serializes UTC instants with zero, three, six or nine fractional digits.
// Lexical comparison at nine digits preserves exact turn boundaries; SQLite's date
// functions round fractions, whereas durable timestamp_ms truncates them.
fn utc_key(stamp: &str) -> String {
    format!(
        "CASE WHEN {stamp} GLOB '????-??-??T??:??:??*Z'
          AND (length({stamp}) = 20 OR (length({stamp}) IN (24,27,30)
               AND substr({stamp},20,1) = '.'
               AND substr({stamp},21,length({stamp})-21) NOT GLOB '*[^0-9]*'))
          AND strftime('%Y-%m-%dT%H:%M:%S',substr({stamp},1,19),'+0 days') = substr({stamp},1,19)
         THEN substr({stamp},1,19) || '.' ||
              substr(substr({stamp},21,max(length({stamp})-21,0)) || '000000000',1,9) END"
    )
}

pub(super) fn project(tx: &Transaction<'_>) -> Result<()> {
    let point_time = utc_key("json_extract(point,'$.timestamp')");
    let turn_start = utc_key("json_extract(turn,'$.started_at')");
    let turn_end = utc_key("json_extract(turn,'$.completed_at')");
    let zero = vector(|_| "0".into());
    let event_matches = matches_vector("p.point -> '$.delta'", |field| format!("e.{field}"));
    let turn_matches = matches_vector("t.turn -> '$.tokens'", |field| {
        format!("coalesce(json_extract(v.original, '$.{field}'),0)")
    });
    let fact_vector = vector(|field| format!("e.{field}"));
    let corrected_sum = sum_vector("corrected");
    let original_sum = sum_vector("original");
    let turn_sum = sum_vector("tokens");
    let session_matches = |object: &str| {
        matches_vector(object, |field| {
            format!("json_extract(total.corrected,'$.{field}')")
        })
    };
    let snapshot_matches = session_matches("d.snapshot -> '$.tokens_total'");
    let summary_matches = session_matches("d.summary -> '$.tokens_total'");

    tx.execute_batch(&format!(r#"
        CREATE TEMP TABLE plateau_projection(
            session_key TEXT PRIMARY KEY, session_json BLOB NOT NULL, summary_json BLOB NOT NULL
        );
        CREATE TEMP VIEW pp_documents AS
        SELECT k.session_key, CAST(s.session_json AS TEXT) AS snapshot, CAST(m.summary_json AS TEXT) AS summary
        FROM plateau_sessions k
        JOIN durable_sessions d USING(session_key)
        JOIN session_snapshots s ON s.session_key=k.session_key AND s.version=d.current_snapshot_version
        JOIN session_summaries m USING(session_key)
        WHERE s.format_version=1
          AND CASE WHEN json_valid(CAST(s.session_json AS TEXT)) AND json_valid(CAST(m.summary_json AS TEXT))
              THEN json_type(s.session_json,'$.tokens_history')='array' AND json_type(s.session_json,'$.turns')='array'
                   AND json_type(m.summary_json)='object' END;

        CREATE TEMP TABLE pp_points AS
        SELECT d.session_key, h.key AS idx, jsonb(h.value) AS point
        FROM pp_documents d, json_each(d.snapshot,'$.tokens_history') h WHERE h.type='object';
        CREATE UNIQUE INDEX pp_points_idx ON pp_points(session_key,idx);

        CREATE TEMP TABLE pp_events AS
        SELECT e.session_key, e.event_index AS idx, p.point, e.model, e.service_tier, e.discarded,
               {point_time} AS stamp, {fact_vector} AS original,
               CASE WHEN e.discarded THEN {zero} ELSE {fact_vector} END AS corrected,
               ({event_matches})
                AND json_type(p.point,'$.total_tokens') IS 'integer'
                AND json_extract(p.point,'$.total_tokens') IS e.cumulative_total_tokens
                AND json_extract(p.point,'$.model') IS e.model
                AND e.model IS NOT NULL
                AND json_extract(p.point,'$.service_tier') IS e.service_tier
                AND json_extract(p.point,'$.request_input_tokens') IS e.request_input_tokens
                AS aligned,
               e.timestamp_ms
        FROM plateau_events e JOIN pp_points p ON p.session_key=e.session_key AND p.idx=e.event_index;
        DROP TABLE pp_points;
        CREATE UNIQUE INDEX pp_events_idx ON pp_events(session_key,idx);
        CREATE INDEX pp_events_time ON pp_events(session_key,stamp);

        CREATE TEMP TABLE pp_turns AS
        WITH raw AS (
            SELECT d.session_key, h.key AS idx, jsonb(h.value) AS turn
            FROM pp_documents d, json_each(d.snapshot,'$.turns') h WHERE h.type='object'
        ), timed AS (
            SELECT *, {turn_start} AS start, {turn_end} AS finish FROM raw
        )
        SELECT *, lead(start) OVER (PARTITION BY session_key ORDER BY idx) AS next_start FROM timed;
        CREATE UNIQUE INDEX pp_turns_idx ON pp_turns(session_key,idx);

        -- The next turn takes ownership at its start, including when the previous
        -- completion timestamp overlaps it. Preserve zero-use turns with no start.
        CREATE TEMP TABLE pp_assigned AS
        SELECT t.session_key,t.idx AS turn_idx,e.idx AS event_idx
        FROM pp_turns t JOIN pp_events e ON e.session_key=t.session_key
          AND e.stamp >= t.start AND e.stamp < coalesce(t.next_start,'~')
          AND e.stamp <= coalesce(t.finish,'~')
        WHERE json_extract(e.original,'$.total_tokens') > 0 OR e.discarded;
        CREATE INDEX pp_assigned_event ON pp_assigned(session_key,event_idx);
        CREATE TEMP TABLE pp_turn_values AS
        SELECT a.session_key,a.turn_idx,{original_sum} AS original,{corrected_sum} AS corrected
        FROM pp_assigned a JOIN pp_events e ON e.session_key=a.session_key AND e.idx=a.event_idx
        GROUP BY a.session_key,a.turn_idx;
        CREATE UNIQUE INDEX pp_turn_values_idx ON pp_turn_values(session_key,turn_idx);

        CREATE TEMP TABLE pp_verified AS
        WITH total AS (SELECT session_key,count(*) AS n,{corrected_sum} AS corrected
                       FROM pp_events GROUP BY session_key)
        SELECT d.session_key FROM pp_documents d JOIN total USING(session_key)
        WHERE json_type(d.snapshot,'$.tokens_total')='object' AND json_type(d.summary,'$.tokens_total')='object'
          AND total.n=json_array_length(d.snapshot,'$.tokens_history')
          AND total.n=(SELECT count(*) FROM plateau_events e WHERE e.session_key=d.session_key)
          AND json_array_length(d.snapshot,'$.turns')=(SELECT count(*) FROM pp_turns t WHERE t.session_key=d.session_key)
          AND (json_type(d.snapshot,'$.legacy_token_deltas') IS NULL
               OR (json_type(d.snapshot,'$.legacy_token_deltas')='object'
               AND NOT EXISTS(SELECT 1 FROM json_each(d.snapshot,'$.legacy_token_deltas'))))
          AND {snapshot_matches} AND {summary_matches}
          AND (SELECT total(json_extract(t.turn,'$.tool_metrics.calls')) FROM pp_turns t
                 WHERE t.session_key=d.session_key) <= 9007199254740991
          AND NOT EXISTS (SELECT 1 FROM pp_events e WHERE e.session_key=d.session_key
             AND (e.aligned IS NOT 1 OR e.stamp IS NULL OR e.timestamp_ms IS NOT
                (unixepoch(substr(e.stamp,1,19))*1000+CAST(substr(e.stamp,21,3) AS INTEGER))
              OR ((json_extract(e.original,'$.total_tokens') > 0 OR e.discarded)
               AND (SELECT count(*) FROM pp_assigned a WHERE a.session_key=e.session_key AND a.event_idx=e.idx) != 1)))
          AND NOT EXISTS (SELECT 1 FROM pp_turns t LEFT JOIN pp_turn_values v
              ON v.session_key=t.session_key AND v.turn_idx=t.idx WHERE t.session_key=d.session_key
              AND ((json_extract(t.turn,'$.started_at') IS NOT NULL AND t.start IS NULL)
                OR (json_extract(t.turn,'$.completed_at') IS NOT NULL AND t.finish IS NULL)
                OR (json_type(t.turn,'$.classification') IS NOT NULL AND
                   (json_type(t.turn,'$.classification') IS NOT 'object' OR
                    coalesce(json_extract(t.turn,'$.classification.category'),'') NOT IN
                      ('planning','exploration','coding','debugging','testing','review','other')))
                OR (json_type(t.turn,'$.tool_metrics') IS NOT NULL AND
                   (json_type(t.turn,'$.tool_metrics.calls') IS NOT 'integer' OR json_extract(t.turn,'$.tool_metrics.calls') < 0))
                OR json_type(t.turn,'$.model') NOT IN ('null','text')
                OR json_type(t.turn,'$.service_tier') NOT IN ('null','text')
                OR NOT ({turn_matches})));
        CREATE UNIQUE INDEX pp_verified_idx ON pp_verified(session_key);

        -- Use each event's model/tier for session pricing, and the turn's model/tier
        -- for category pricing, as in Session::tier_buckets and telemetry::category_totals.
        WITH events AS (
            SELECT e.* FROM pp_events e JOIN pp_verified USING(session_key)
        ), history AS (
            SELECT session_key,json_group_array(json(CASE WHEN discarded THEN
                       json_set(point,'$.delta',json(corrected)) ELSE point END) ORDER BY idx) AS points
            FROM events GROUP BY session_key
        ), legacy AS (
            SELECT session_key,json_group_object(CAST(idx AS TEXT),
                 json_object('model_was_missing',json('false'),'delta',point -> '$.delta')) AS deltas
            FROM events WHERE discarded GROUP BY session_key
        ), model_values AS (
            SELECT session_key,model,{corrected_sum} AS tokens FROM events GROUP BY session_key,model
        ), models AS (
            SELECT session_key,json_group_object(model,json(tokens)) AS by_model FROM model_values GROUP BY session_key
        ), tier_values AS (
            SELECT session_key,model,service_tier,{corrected_sum} AS tokens FROM events GROUP BY session_key,model,service_tier
        ), tiers AS (
            SELECT session_key,json_group_array(json_object('model',model,'service_tier',service_tier,
                'tokens',json(tokens)) ORDER BY model,service_tier) AS buckets FROM tier_values GROUP BY session_key
        ), turns AS (
            SELECT t.session_key,t.idx,t.turn,coalesce(v.corrected,{zero}) AS tokens,
                   coalesce(json_extract(t.turn,'$.classification.category'),'other') AS category,
                   json_extract(t.turn,'$.model') AS model,json_extract(t.turn,'$.service_tier') AS service_tier,
                   coalesce(json_extract(t.turn,'$.tool_metrics.calls'),0) AS calls
            FROM pp_turns t JOIN pp_verified USING(session_key) LEFT JOIN pp_turn_values v
                 ON v.session_key=t.session_key AND v.turn_idx=t.idx
        ), turn_arrays AS (
            SELECT session_key,json_group_array(json_set(turn,'$.tokens',json(tokens)) ORDER BY idx) AS items
            FROM turns GROUP BY session_key
        ), category_tiers AS (
            SELECT session_key,category,model,service_tier,{turn_sum} AS tokens FROM turns WHERE model IS NOT NULL
            GROUP BY session_key,category,model,service_tier
        ), category_buckets AS (
            SELECT session_key,category,json_group_array(json_object('model',model,'service_tier',service_tier,
                   'tokens',json(tokens)) ORDER BY model,service_tier) AS buckets
            FROM category_tiers GROUP BY session_key,category
        ), category_values AS (
            SELECT session_key,category,count(*) AS n,sum(calls) AS calls,{turn_sum} AS tokens
            FROM turns GROUP BY session_key,category
        ), categories AS (
            SELECT c.session_key,json_group_object(c.category,json_object('turns',c.n,'tokens',json(c.tokens),
                   'tool_calls',c.calls,'buckets',json(coalesce(b.buckets,'[]')))) AS metrics
            FROM category_values c LEFT JOIN category_buckets b USING(session_key,category) GROUP BY c.session_key
        )
        INSERT INTO plateau_projection
        SELECT d.session_key,
               CAST(json_set(d.snapshot,'$.tokens_history',json(h.points),'$.legacy_token_deltas',json(l.deltas),
                         '$.turns',json(t.items),'$.tokens_by_model',json(m.by_model),'$.category_totals',json(c.metrics)) AS BLOB),
               CAST(json_set(d.summary,'$.buckets',json(b.buckets),'$.category_totals',json(c.metrics)) AS BLOB)
        FROM pp_documents d JOIN history h USING(session_key) JOIN legacy l USING(session_key)
        JOIN turn_arrays t USING(session_key) JOIN models m USING(session_key) JOIN tiers b USING(session_key)
        JOIN categories c USING(session_key);

        DROP TABLE pp_verified;
        DROP TABLE pp_turn_values;
        DROP TABLE pp_assigned;
        DROP TABLE pp_turns;
        DROP TABLE pp_events;
        DROP VIEW pp_documents;
    "#))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::{params, Connection};
    use serde_json::{json, Value};

    fn tokens(n: i64) -> Value {
        json!({"input_tokens":n,"cached_input_tokens":0,"output_tokens":0,
               "reasoning_output_tokens":0,"total_tokens":n})
    }

    #[test]
    fn projection_proves_turn_ownership_before_repricing_categories() {
        let mut db = Connection::open_in_memory().unwrap();
        let tx = db.transaction().unwrap();
        tx.execute_batch("
            CREATE TABLE durable_sessions(session_key TEXT PRIMARY KEY,current_snapshot_version INTEGER);
            CREATE TABLE session_snapshots(session_key TEXT,version INTEGER,format_version INTEGER,session_json BLOB);
            CREATE TABLE session_summaries(session_key TEXT,summary_json BLOB);
            CREATE TEMP TABLE plateau_sessions(session_key TEXT PRIMARY KEY);
            CREATE TEMP TABLE plateau_events(session_key TEXT,event_index INTEGER,timestamp_ms INTEGER,
                model TEXT,service_tier TEXT,request_input_tokens INTEGER,cumulative_total_tokens INTEGER,
                input_tokens INTEGER,cached_input_tokens INTEGER,cache_creation_input_tokens INTEGER,
                output_tokens INTEGER,reasoning_output_tokens INTEGER,total_tokens INTEGER,discarded INTEGER);
        ").unwrap();
        for key in [
            "verified",
            "mismatched-turn",
            "invalid-category",
            "duplicate-turn-keys",
            "untrusted-delta",
        ] {
            let mut points = Vec::new();
            for (idx, (stamp, ms, model, n, cumulative, discard)) in [
                ("2020-01-01T00:00:00.000999999Z", 0, "event-a", 5, 5, false),
                (
                    "2020-01-01T00:00:02.123999999Z",
                    2123,
                    "event-a",
                    5,
                    5,
                    true,
                ),
                ("2020-01-01T00:00:03Z", 3000, "event-b", 3, 8, false),
            ]
            .into_iter()
            .enumerate()
            {
                points.push(json!({"timestamp":stamp,"model":model,"service_tier":null,
                    "total_tokens":cumulative,"delta":tokens(n),"note":"preserve"}));
                tx.execute("INSERT INTO plateau_events VALUES(?1,?2,1577836800000+?3,?4,NULL,NULL,?5,?6,0,0,0,0,?6,?7)",
                    params![key,idx as i64,ms,model,cumulative,n,discard]).unwrap();
            }
            let mut second = tokens(8);
            if key == "mismatched-turn" {
                second["cached_input_tokens"] = json!(1);
            }
            let mut snapshot = json!({"tokens_history":points,"tokens_total":tokens(8),"custom":"preserve","turns":[
                {"started_at":"2020-01-01T00:00:00Z","completed_at":"2020-01-01T00:00:03Z",
                 "classification":{"category":"coding"},"model":"first-turn","tokens":tokens(5)},
                {"started_at":"2020-01-01T00:00:02.123999999Z","classification":{"category":"coding"},
                 "model":"second-turn","service_tier":"fast","tokens":second,"tool_metrics":{"calls":4}}
            ]});
            match key {
                "invalid-category" => snapshot["turns"][0]["classification"]["category"] = json!(5),
                "untrusted-delta" => {
                    snapshot["tokens_history"][0]["delta"]["input_tokens"] = json!(i64::MAX)
                }
                _ => {}
            }
            let raw = if key == "duplicate-turn-keys" {
                r#"{"turns":{"a":{},"a":{}},"tokens_history":[]}"#.to_owned()
            } else {
                snapshot.to_string()
            };
            tx.execute("INSERT INTO plateau_sessions VALUES(?1)", [key])
                .unwrap();
            tx.execute("INSERT INTO durable_sessions VALUES(?1,1)", [key])
                .unwrap();
            tx.execute(
                "INSERT INTO session_snapshots VALUES(?1,1,1,?2)",
                params![key, raw.as_bytes()],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO session_summaries VALUES(?1,?2)",
                params![
                    key,
                    json!({"tokens_total":tokens(8),"custom":"preserve"})
                        .to_string()
                        .as_bytes()
                ],
            )
            .unwrap();
        }
        project(&tx).unwrap();
        let (n, key, raw, summary): (i64, String, Vec<u8>, Vec<u8>) = tx
            .query_row(
                "SELECT count(*),session_key,session_json,summary_json FROM plateau_projection",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!((n, key.as_str()), (1, "verified"));
        let snapshot: Value = serde_json::from_slice(&raw).unwrap();
        let summary: Value = serde_json::from_slice(&summary).unwrap();
        assert_eq!(snapshot["custom"], "preserve");
        assert_eq!(snapshot["tokens_history"][1]["note"], "preserve");
        assert_eq!(snapshot["tokens_history"][1]["delta"]["input_tokens"], 0);
        assert_eq!(
            snapshot["legacy_token_deltas"]["1"],
            json!({"model_was_missing":false,"delta":tokens(5)})
        );
        assert_eq!(snapshot["turns"][0]["tokens"]["input_tokens"], 5);
        assert_eq!(snapshot["turns"][1]["tokens"]["input_tokens"], 3);
        assert_eq!(snapshot["tokens_by_model"]["event-a"]["input_tokens"], 5);
        let category = &snapshot["category_totals"]["coding"];
        assert_eq!(category["tokens"]["input_tokens"], 8);
        assert_eq!(category["tool_calls"], 4);
        assert_eq!(category["buckets"][1]["model"], "second-turn");
        assert_eq!(category["buckets"][1]["service_tier"], "fast");
        assert_eq!(category["buckets"][1]["tokens"]["input_tokens"], 3);
        assert_eq!(summary["category_totals"], snapshot["category_totals"]);
        assert_eq!(summary["custom"], "preserve");
        assert_eq!(summary["buckets"][1]["model"], "event-b");
    }
}
