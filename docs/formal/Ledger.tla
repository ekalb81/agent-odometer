------------------------------- MODULE Ledger -------------------------------
EXTENDS Integers, Sequences, FiniteSets

\* One watched path, two provider/fingerprint identities and three snapshots.
\* Ticks are inclusive; each simulated hour contains two ticks.
Keys == {"a", "b"}
Versions == 1..3
Ticks == 0..5
Hours == 0..2
Dimensions == 1..7
Key(v) == IF v = 3 THEN "b" ELSE "a"
Max(a, b) == IF a > b THEN a ELSE b

\* Reference events store disjoint portions: uncached, cached, short creation,
\* long creation, ordinary output, reasoning, and an independent tool count.
\* SQLite exposes combined creation tokens, so TTL portions are oracle inputs.
Event(t, n) == [time |-> t, parts |-> <<n, n+1, n+2, n+3, n+4, n+5, n+6>>]
Events(v) ==
    CASE v = 0 -> <<>>
      [] v = 1 -> <<Event(0, 1), Event(1, 2), Event(2, 3)>>
      [] v = 2 -> <<Event(0, 1), Event(1, 2), Event(2, 3), Event(4, 4)>>
      [] v = 3 -> <<Event(4, 5), Event(5, 6)>>

\* Ledger dimensions: combined input, cached read, combined creation, combined
\* output, reasoning, total tokens, and tool count (not part of token total).
Fact(e) ==
    LET p == e.parts
        created == p[3] + p[4]
        input == p[1] + p[2] + created
        output == p[5] + p[6]
    IN [time |-> e.time, amount |-> <<input, p[2], created, output,
                                      p[6], input + output, p[7]>>]
Facts(v) == LET es == Events(v) IN [i \in DOMAIN es |-> Fact(es[i])]

RECURSIVE Sum(_, _)
Sum(rows, d) == IF rows = <<>> THEN 0 ELSE Head(rows)[d] + Sum(Tail(rows), d)
Bucket(t) == t \div 2
Aggregate(rows) ==
    [h \in Hours |-> LET facts == SelectSeq(rows, LAMBDA f: Bucket(f.time) = h)
                     IN [d \in Dimensions |-> Sum([i \in DOMAIN facts |-> facts[i].amount], d)]]
RECURSIVE AddHours(_, _)
AddHours(rows, totals) ==
    IF rows = <<>> THEN totals
    ELSE LET f == Head(rows)
             h == Bucket(f.time)
         IN AddHours(Tail(rows), [totals EXCEPT ![h] =
                                    [d \in Dimensions |-> totals[h][d] + f.amount[d]]])

Windows == {<<lo, hi>> \in Ticks \X Ticks : lo <= hi}
InWindow(w, t) == w[1] <= t /\ t <= w[2]
FullHour(w, h) == w[1] <= 2*h /\ 2*h + 1 <= w[2]
QueryRows(rows, hours, w) ==
    LET edges == SelectSeq(rows, LAMBDA f: InWindow(w, f.time) /\ ~FullHour(w, Bucket(f.time)))
        selected == [i \in DOMAIN edges |-> edges[i].amount]
        rolled == [i \in 1..3 |-> IF FullHour(w, i-1) THEN hours[i-1]
                                  ELSE [d \in Dimensions |-> 0]]
    IN [d \in Dimensions |-> Sum(selected, d) + Sum(rolled, d)]

\* The raw-event oracle filters by timestamp before deriving output dimensions.
Oracle(v, w) ==
    LET es == SelectSeq(Events(v), LAMBDA e: InWindow(w, e.time))
        parts == [i \in DOMAIN es |-> es[i].parts]
        p == [d \in Dimensions |-> Sum(parts, d)]
    IN <<p[1]+p[2]+p[3]+p[4], p[2], p[3]+p[4], p[5]+p[6], p[6],
         p[1]+p[2]+p[3]+p[4]+p[5]+p[6], p[7]>>

VARIABLES db, source, generation, seen, policyRevision, orgRevision, review,
          journal, verifiedJournal, phase, reader, scenario, lostEvents
vars == <<db, source, generation, seen, policyRevision, orgRevision, review,
          journal, verifiedJournal, phase, reader, scenario, lostEvents>>
internals == <<scenario, lostEvents>>

NoKey == "none"
NoReview == [cutoff |-> 0]
NoRead == [status |-> "none"]
Unavailable == <<>>
EmptyRow == [snapshot |-> 0, lifecycle |-> "absent",
             facts |-> <<>>, hours |-> Aggregate(<<>>)]
EmptyDB ==
    [rows |-> [k \in Keys |-> EmptyRow], pending |-> {}, stale |-> FALSE,
     present |-> NoKey,
     tombstones |-> {}, complete |-> TRUE, lossBound |-> -1, lossCount |-> 0,
     lossRevision |-> 0]
Init ==
    /\ db = EmptyDB
    /\ source = 0
    /\ generation = 0
    /\ seen = 0
    /\ policyRevision = 0
    /\ orgRevision = 0
    /\ review = NoReview
    /\ journal = {}
    /\ verifiedJournal = TRUE
    /\ phase = "ready"
    /\ reader = NoRead
    \* Keep scan generations, reviewed purge/read races, and recovery in
    \* separate scenarios to avoid multiplying unrelated state dimensions.
    /\ scenario \in {"scan", "retention", "recovery"}
    /\ lostEvents = {}

Usable == phase \in {"ready", "rebuilt"}

\* Same-fingerprint replay never appends duplicate facts. A shorter source can
\* restore availability while the latest snapshot still owns accounting.
Observed(d, v, deferred) ==
    LET k == Key(v)
        previous == d.present
        existing == d.rows[k]
        fresh == v > existing.snapshot
        facts == IF fresh THEN Facts(v) ELSE existing.facts
        row == [snapshot |-> Max(v, existing.snapshot), lifecycle |-> "present",
                facts |-> facts, hours |-> IF fresh /\ ~deferred THEN AddHours(facts, EmptyRow.hours)
                                         ELSE existing.hours]
    IN [d EXCEPT !.present = k,
                 !.rows = [j \in Keys |-> IF j = k THEN row
                          ELSE IF j = previous THEN [d.rows[j] EXCEPT !.lifecycle = "superseded"]
                          ELSE d.rows[j]],
                 !.pending = IF fresh /\ deferred THEN @ \cup {k} ELSE @,
                 !.stale = @ \/ (fresh /\ deferred)]

Observe(v, g, deferred) ==
    /\ Usable
    /\ source' = v
    /\ IF Key(v) \in db.tombstones
       THEN UNCHANGED <<db, seen>>
       ELSE /\ db' = Observed(db, v, deferred)
            /\ seen' = Max(seen, g)
    /\ UNCHANGED <<generation, policyRevision, orgRevision, review, journal,
                   verifiedJournal, phase, reader>>

Missing(d) ==
    [d EXCEPT !.present = NoKey, !.rows[d.present].lifecycle = "retained"]
Remove ==
    /\ Usable /\ source # 0
    /\ source' = 0
    /\ db' = IF db.present = NoKey THEN db ELSE Missing(db)
    /\ UNCHANGED <<generation, seen, policyRevision, orgRevision, review,
                   journal, verifiedJournal, phase, reader>>

StartScan ==
    /\ Usable /\ scenario = "scan" /\ generation < 2
    /\ generation' = generation + 1
    /\ UNCHANGED <<db, source, seen, policyRevision, orgRevision, review,
                   journal, verifiedJournal, phase, reader>>
FinishScan(g) ==
    /\ Usable /\ generation = g /\ seen < g /\ db.present # NoKey
    /\ db' = Missing(db)
    /\ UNCHANGED <<source, generation, seen, policyRevision, orgRevision,
                   review, journal, verifiedJournal, phase, reader>>

\* SQL backfill and deferred rollup repair share an atomic publication step.
Backfill ==
    /\ Usable /\ db.stale
    /\ db' = [db EXCEPT !.pending = {}, !.stale = FALSE,
              !.rows = [k \in Keys |-> [db.rows[k] EXCEPT !.hours = Aggregate(db.rows[k].facts)]]]
    /\ UNCHANGED <<source, generation, seen, policyRevision, orgRevision,
                   review, journal, verifiedJournal, phase, reader>>

\* Policy revision 0 keeps all history; 1 and 2 represent saving the same
\* finite-retention policy twice. Changing only a private edit invalidates review.
LastTick(v) == IF v = 0 THEN -1 ELSE Events(v)[Len(Events(v))].time
Candidates(cutoff) ==
    {k \in Keys : /\ db.rows[k].lifecycle \in {"retained", "superseded"}
                  /\ LastTick(db.rows[k].snapshot) < cutoff}
Signature(cutoff) ==
    [cutoff |-> cutoff, policy |-> policyRevision, organization |-> orgRevision,
     rows |-> {<<k, db.rows[k].snapshot, db.rows[k].lifecycle>> : k \in Candidates(cutoff)}]
SavePolicy ==
    /\ Usable /\ scenario # "scan" /\ policyRevision < 2
    /\ policyRevision' = policyRevision + 1
    /\ UNCHANGED <<db, source, generation, seen, orgRevision, review, journal,
                   verifiedJournal, phase, reader>>
EditPrivate ==
    /\ Usable /\ scenario = "retention" /\ orgRevision = 0
    /\ orgRevision' = 1
    /\ UNCHANGED <<db, source, generation, seen, policyRevision, review, journal,
                   verifiedJournal, phase, reader>>
Preview(cutoff) ==
    /\ Usable /\ policyRevision > 0
    /\ review' = Signature(cutoff)
    /\ UNCHANGED <<db, source, generation, seen, policyRevision, orgRevision,
                   journal, verifiedJournal, phase, reader>>

Latest(ts) == CHOOSE end \in ts : \A t \in ts : t <= end
EventsLost(keys) ==
    UNION {LET es == Events(db.rows[k].snapshot)
           IN {es[i].time : i \in DOMAIN es} : k \in keys}
Loss(keys) ==
    Latest(EventsLost(keys) \cup UNION
        {{2*h + 1 : h \in {i \in Hours : db.rows[k].hours[i][6] > 0}} : k \in keys})
Purged(keys) ==
    [db EXCEPT !.rows = [k \in Keys |-> IF k \in keys THEN EmptyRow ELSE db.rows[k]],
               !.pending = @ \ keys,
               !.tombstones = @ \cup keys, !.complete = FALSE,
               !.lossBound = IF phase = "ready" THEN Max(@, Loss(keys)) ELSE -1,
               !.lossCount = IF phase = "ready" THEN Cardinality(db.tombstones \cup keys) ELSE 0,
               !.lossRevision = IF phase = "ready" THEN @ + 1 ELSE 0]
Confirm(commit) ==
    /\ Usable /\ review.cutoff # 0 /\ policyRevision > 0
    /\ review.cutoff <= 6 /\ review = Signature(review.cutoff)
    /\ LET keys == Candidates(review.cutoff)
       IN /\ journal' = journal \cup keys
          /\ db' = IF commit /\ keys # {} THEN Purged(keys) ELSE db
          /\ lostEvents' = IF commit THEN lostEvents \cup EventsLost(keys) ELSE lostEvents
    /\ review' = NoReview
    /\ UNCHANGED <<source, generation, seen, policyRevision, orgRevision,
                   verifiedJournal, phase, reader>>

\* Preserve the unavailable original before reconstructing readable inputs.
\* An interrupted recovery can remain at either marker/preservation step.
Corrupt(valid) ==
    /\ phase = "ready" /\ scenario = "recovery"
    /\ phase' = "corrupt"
    /\ verifiedJournal' = valid
    /\ UNCHANGED <<db, source, generation, seen, policyRevision, orgRevision,
                   review, journal, reader>>
RecoveryStep ==
    /\ phase \in {"corrupt", "marked"}
    /\ phase' = IF phase = "corrupt" THEN "marked" ELSE "preserved"
    /\ UNCHANGED <<db, source, generation, seen, policyRevision, orgRevision,
                   review, journal, verifiedJournal, reader>>
Recover ==
    /\ phase = "preserved" /\ verifiedJournal
    /\ phase' = "rebuilt"
    /\ LET clean == [EmptyDB EXCEPT !.complete = FALSE, !.tombstones = journal]
       IN db' = IF source = 0 \/ Key(source) \in journal THEN clean
                ELSE Observed(clean, source, FALSE)
    /\ review' = NoReview
    /\ UNCHANGED <<source, generation, seen, policyRevision, orgRevision,
                   journal, verifiedJournal, reader>>

Coverage(d, state, valid, from) ==
    valid /\ state = "ready" /\
    ((d.complete /\ d.tombstones = {}) \/
     (~d.complete /\ d.lossBound >= 0 /\ from > d.lossBound /\
      d.lossCount = Cardinality(d.tombstones) /\ d.lossRevision > 0))

\* This read pins a single snapshot before any writer can publish. Deliver can
\* run after replacement, backfill or purge, but consumes only the pinned rows.
ReadWindow == <<1, 4>>
BeginRead ==
    /\ Usable /\ scenario # "recovery" /\ reader.status = "none"
    /\ LET row == db.rows["a"]
       IN /\ row.snapshot = 2
          /\ reader' = [facts |-> row.facts, hours |-> row.hours,
                  admitted |-> ~db.stale,
                  expected |-> Oracle(row.snapshot, ReadWindow),
                  status |-> "waiting", result |-> Unavailable]
    /\ UNCHANGED <<db, source, generation, seen, policyRevision, orgRevision,
                   review, journal, verifiedJournal, phase>>
Deliver ==
    /\ reader.status = "waiting"
    /\ reader' = [reader EXCEPT !.status = "delivered", !.result = IF reader.admitted
                   THEN QueryRows(reader.facts, reader.hours, ReadWindow)
                   ELSE Unavailable]
    /\ UNCHANGED <<db, source, generation, seen, policyRevision, orgRevision,
                   review, journal, verifiedJournal, phase>>
CloseRead ==
    /\ reader.status = "delivered"
    /\ reader' = NoRead
    /\ UNCHANGED <<db, source, generation, seen, policyRevision, orgRevision,
                   review, journal, verifiedJournal, phase>>

OtherStep ==
    \/ \E v \in Versions, g \in 0..generation, deferred \in BOOLEAN : Observe(v, g, deferred)
    \/ Remove \/ StartScan \/ \E g \in 0..generation : FinishScan(g)
    \/ Backfill \/ SavePolicy \/ EditPrivate
    \/ \E cutoff \in {4, 6} : Preview(cutoff)
    \/ \E valid \in BOOLEAN : Corrupt(valid)
    \/ RecoveryStep \/ Recover
    \/ BeginRead
    \/ Deliver \/ CloseRead
Next ==
    \/ OtherStep /\ UNCHANGED internals
    \/ (\E commit \in BOOLEAN : Confirm(commit)) /\ UNCHANGED scenario
Spec == Init /\ [][Next]_vars

LifecycleSound ==
    /\ db.tombstones \subseteq journal
    /\ db.present \in Keys \cup {NoKey}
    /\ \A k \in Keys :
       /\ (k = db.present) = (db.rows[k].lifecycle = "present")
       /\ (db.rows[k].snapshot = 0) = (db.rows[k].lifecycle = "absent")
       /\ k \in db.tombstones => db.rows[k] = EmptyRow /\ k \notin db.pending

MaterializationSound ==
    /\ ~db.stale => db.pending = {}
    /\ \A k \in Keys :
        LET row == db.rows[k]
        IN /\ row.facts = Facts(row.snapshot)
           /\ k \notin db.pending => row.hours = Aggregate(row.facts)

WindowSound ==
    Usable /\ ~db.stale =>
        \A k \in Keys, w \in Windows :
            LET row == db.rows[k]
            IN QueryRows(row.facts, row.hours, w) = Oracle(row.snapshot, w)

ReadSound ==
    reader.status = "delivered" =>
        reader.result = (IF reader.admitted THEN reader.expected ELSE Unavailable)

CoverageSound ==
    /\ db.complete => db.tombstones = {}
    /\ phase = "rebuilt" => ~db.complete /\ verifiedJournal
    /\ \A from \in 0..6 :
       Coverage(db, phase, verifiedJournal, from) =>
           /\ \A t \in lostEvents : t < from
           /\ db.tombstones # {} => from > db.lossBound

PurgeReviewSound ==
    [][(Usable /\ (db'.tombstones # db.tombstones \/ journal' # journal)) =>
        /\ source' = source
        /\ review.cutoff \in {4, 6}
        /\ review = Signature(review.cutoff)
        /\ journal' = journal \cup Candidates(review.cutoff)
        /\ db'.tombstones \in {db.tombstones, db.tombstones \cup Candidates(review.cutoff)}]_vars
ScanGenerationSound ==
    [][(Usable /\ source' = source /\ db.present # NoKey /\ db'.present = NoKey) =>
       seen < generation]_vars
RecoverySound ==
    [][(phase # "rebuilt" /\ phase' = "rebuilt") =>
        /\ phase = "preserved" /\ verifiedJournal
        /\ db'.tombstones = journal /\ ~db'.complete
        /\ \A k \in Keys :
             db'.rows[k].snapshot =
                 IF source # 0 /\ k = Key(source) /\ k \notin journal THEN source ELSE 0]_vars
=============================================================================
