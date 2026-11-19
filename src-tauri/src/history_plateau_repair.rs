//! Repair retained Codex accounting only when the archived event sequence and
//! its independent cumulative, snapshot, and turn totals prove the correction.
use anyhow::Result;
use rusqlite::Transaction;

pub(super) fn repair(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE TEMP TABLE plateau_sessions(session_key TEXT PRIMARY KEY) WITHOUT ROWID;
         INSERT INTO plateau_sessions
         SELECT d.session_key FROM durable_sessions d
         JOIN session_snapshots s ON s.session_key=d.session_key AND s.version=d.current_snapshot_version
         JOIN session_summaries u ON u.session_key=d.session_key
         WHERE d.ledger_dirty=1 AND d.lifecycle='retained' AND d.fingerprint_is_final=1
           AND d.session_key GLOB 'codex:thread:*' AND s.format_version=1
           AND d.current_snapshot_hash=s.snapshot_hash
           AND EXISTS(SELECT 1 FROM source_locations l WHERE l.session_key=d.session_key)
           AND NOT EXISTS(SELECT 1 FROM source_locations l LEFT JOIN source_artifacts a USING(artifact_key)
                 WHERE l.session_key=d.session_key AND (l.present!=0 OR a.session_key IS NOT d.session_key
                    OR a.identity_key IS NOT d.identity_key OR a.first_event_fingerprint IS NOT d.first_event_fingerprint))
           AND NOT EXISTS(SELECT 1 FROM source_artifacts a WHERE a.session_key=d.session_key
                 AND (a.identity_key IS NOT d.identity_key OR a.first_event_fingerprint IS NOT d.first_event_fingerprint))
           AND NOT EXISTS(SELECT 1 FROM source_artifacts a JOIN source_locations l USING(artifact_key)
                 WHERE a.session_key=d.session_key AND l.session_key!=d.session_key)
           AND NOT EXISTS(SELECT 1 FROM purged_sessions p WHERE p.identity_key=d.identity_key
                 AND p.first_event_fingerprint=d.first_event_fingerprint)
           AND CASE WHEN length(s.session_json)<=67108864 AND length(u.summary_json)<=67108864
                      AND json_valid(CAST(s.session_json AS TEXT)) AND json_valid(CAST(u.summary_json AS TEXT))
                    THEN json_extract(CAST(s.session_json AS TEXT),'$.harness')='codex'
                     AND json_extract(CAST(u.summary_json AS TEXT),'$.harness')='codex'
                     AND d.identity_key = 'codex:thread:' || json_extract(CAST(s.session_json AS TEXT),'$.id')
                     AND json_extract(CAST(s.session_json AS TEXT),'$.id')=json_extract(CAST(u.summary_json AS TEXT),'$.id')
                     AND json_extract(CAST(s.session_json AS TEXT),'$.storage_id')=d.session_key
                     AND json_extract(CAST(u.summary_json AS TEXT),'$.storage_id')=d.session_key
                     AND json_type(CAST(s.session_json AS TEXT),'$.legacy_token_deltas') IS NULL
                    ELSE 0 END;

         -- A metadata overlay can also mark the ledger dirty when it carries
         -- additional tool observations or findings. This repair handles tokens only.
         WITH findings AS (
           SELECT f.session_key,COUNT(*) AS n FROM durable_finding_events f
           JOIN plateau_sessions k USING(session_key) GROUP BY f.session_key
         ), mismatches AS (
           SELECT k.session_key FROM plateau_sessions k
           JOIN durable_sessions d USING(session_key)
           JOIN session_snapshots s ON s.session_key=k.session_key AND s.version=d.current_snapshot_version
           LEFT JOIN findings f ON f.session_key=k.session_key
           WHERE COALESCE(json_type(CAST(s.session_json AS TEXT),'$.tool_observations'),'array')!='array'
              OR COALESCE(json_type(CAST(s.session_json AS TEXT),'$.optimization_findings'),'array')!='array'
              OR COALESCE(json_array_length(CAST(s.session_json AS TEXT),'$.tool_observations'),0)
                    != (SELECT COUNT(*) FROM durable_tool_events e WHERE e.session_key=k.session_key)
              OR COALESCE(json_array_length(CAST(s.session_json AS TEXT),'$.optimization_findings'),0)!=COALESCE(f.n,0)
         )
         DELETE FROM plateau_sessions WHERE session_key IN (SELECT session_key FROM mismatches);

         CREATE TEMP TABLE plateau_events AS
         SELECT e.*,
                LAG(cumulative_total_tokens,1,0) OVER (PARTITION BY e.session_key ORDER BY event_index) AS previous,
                0 AS discarded
         FROM plateau_sessions k JOIN durable_token_events e USING(session_key);
         UPDATE plateau_events SET discarded=1 WHERE cumulative_total_tokens=previous AND
               (input_tokens>0 OR cached_input_tokens>0 OR cache_creation_input_tokens>0 OR output_tokens>0
                OR reasoning_output_tokens>0 OR total_tokens>0);
         CREATE INDEX temp.plateau_event_sort ON plateau_events(session_key,event_index);",
    )?;

    // TOTAL avoids SQLite integer overflow on corrupt rows. Restrict proofs
    // to nonnegative counters within the exact integer range of binary64.
    tx.execute_batch(
        "WITH proven AS (
           SELECT e.session_key,
             json_object('total_tokens',CAST(TOTAL(IIF(discarded,0,total_tokens)) AS INTEGER),
                         'input_tokens',CAST(TOTAL(IIF(discarded,0,input_tokens)) AS INTEGER),
                         'cached_input_tokens',CAST(TOTAL(IIF(discarded,0,cached_input_tokens)) AS INTEGER),
                         'cache_creation_input_tokens',CAST(TOTAL(IIF(discarded,0,cache_creation_input_tokens)) AS INTEGER),
                         'output_tokens',CAST(TOTAL(IIF(discarded,0,output_tokens)) AS INTEGER),
                         'reasoning_output_tokens',CAST(TOTAL(IIF(discarded,0,reasoning_output_tokens)) AS INTEGER)) AS totals
           FROM plateau_events e GROUP BY e.session_key
           HAVING MIN(event_index)=0 AND MAX(event_index)=COUNT(*)-1 AND COUNT(DISTINCT event_index)=COUNT(*)
             AND MAX(discarded)=1
             AND MAX(TOTAL(total_tokens),TOTAL(input_tokens),TOTAL(output_tokens))<=9007199254740991
             AND MIN(length(event_key)=16 AND event_key NOT GLOB '*[^0-9a-f]*'
                 AND typeof(event_index)='integer' AND typeof(timestamp_ms)='integer'
                 AND typeof(cumulative_total_tokens)='integer' AND typeof(total_tokens)='integer'
                 AND typeof(input_tokens)='integer' AND typeof(cached_input_tokens)='integer'
                 AND typeof(cache_creation_input_tokens)='integer' AND typeof(output_tokens)='integer'
                 AND typeof(reasoning_output_tokens)='integer' AND typeof(model)='text' AND length(model)>0
                 AND typeof(service_tier) IN ('null','text') AND typeof(request_input_tokens) IN ('null','integer')
                 AND COALESCE(request_input_tokens,0)>=0
                 AND cumulative_total_tokens BETWEEN previous AND 9007199254740991
                 AND total_tokens>=0 AND input_tokens>=0 AND output_tokens>=0
                 AND cached_input_tokens>=0 AND cache_creation_input_tokens>=0 AND reasoning_output_tokens>=0
                 AND cached_input_tokens+cache_creation_input_tokens<=input_tokens AND reasoning_output_tokens<=output_tokens
                 AND (cumulative_total_tokens=previous OR (total_tokens=cumulative_total_tokens-previous
                        AND total_tokens=input_tokens+output_tokens)))=1
         ), matched AS (
           SELECT p.session_key FROM proven p
           JOIN session_snapshots s USING(session_key) JOIN session_summaries u USING(session_key)
           JOIN durable_sessions d ON d.session_key=p.session_key AND s.version=d.current_snapshot_version
           WHERE NOT EXISTS(
              SELECT 1 FROM json_each(p.totals) f
              WHERE NOT ((json_type(CAST(s.session_json AS TEXT),'$.tokens_total.'||f.key) IS 'integer'
                         AND json_extract(CAST(s.session_json AS TEXT),'$.tokens_total.'||f.key) IS f.value)
                      OR (f.key='cache_creation_input_tokens' AND f.value=0
                         AND json_type(CAST(s.session_json AS TEXT),'$.tokens_total.'||f.key) IS NULL))
                 OR NOT ((json_type(CAST(u.summary_json AS TEXT),'$.tokens_total.'||f.key) IS 'integer'
                         AND json_extract(CAST(u.summary_json AS TEXT),'$.tokens_total.'||f.key) IS f.value)
                      OR (f.key='cache_creation_input_tokens' AND f.value=0
                         AND json_type(CAST(u.summary_json AS TEXT),'$.tokens_total.'||f.key) IS NULL)))
         )
         DELETE FROM plateau_sessions WHERE session_key NOT IN (SELECT session_key FROM matched);
         DELETE FROM plateau_events WHERE session_key NOT IN (SELECT session_key FROM plateau_sessions);",
    )?;
    super::plateau_projection::project(tx)?;
    // Keep legacy fact keys/indexes and snapshot format for a returning
    // transcript. The hash marker invalidates snapshots and old purge previews.
    tx.execute_batch(
        "UPDATE durable_token_events AS e SET input_tokens=0,cached_input_tokens=0,cache_creation_input_tokens=0,
             output_tokens=0,reasoning_output_tokens=0,total_tokens=0
         FROM plateau_events p JOIN plateau_projection vetted USING(session_key)
         WHERE p.discarded=1 AND e.session_key=p.session_key AND e.event_key=p.event_key;
         UPDATE session_snapshots AS s SET session_json=CAST(p.session_json AS BLOB),
                    snapshot_hash='v18:'||s.snapshot_hash
         FROM plateau_projection p JOIN durable_sessions d USING(session_key)
         WHERE s.session_key=p.session_key AND s.version=d.current_snapshot_version;
         UPDATE session_summaries AS s SET summary_json=CAST(p.summary_json AS BLOB)
         FROM plateau_projection p WHERE s.session_key=p.session_key;
         DELETE FROM rollup_token_totals WHERE session_key IN (SELECT session_key FROM plateau_projection);",
    )?;
    tx.execute_batch(&super::token_rollup_insert(
        "WHERE session_key IN (SELECT session_key FROM plateau_projection)",
    ))?;
    tx.execute_batch(
        "UPDATE durable_sessions SET current_snapshot_hash='v18:'||current_snapshot_hash,ledger_dirty=0
         WHERE session_key IN (SELECT session_key FROM plateau_projection);
         DROP TABLE temp.plateau_projection;
         DROP TABLE temp.plateau_events;
         DROP TABLE temp.plateau_sessions;",
    )?;
    Ok(())
}
