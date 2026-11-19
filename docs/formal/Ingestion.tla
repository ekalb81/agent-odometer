---------------------------- MODULE Ingestion ------------------------------
EXTENDS Naturals, Sequences, FiniteSets

\* All fixtures reuse the same provider session ID. Compare Codex to Claude,
\* Claude to its child, and Gemini to Codex in separate initial states.
Domains == {"codex", "claude", "gemini", "claudeChild"}
Other(p) == CASE p = "codex" -> "claude" [] p = "claude" -> "claudeChild"
              [] p = "gemini" -> "codex" [] OTHER -> "claude"
Inputs == {"prefix", "main", "copy", "fork", "foreign"}
Path(i) == IF i = "prefix" THEN "main" ELSE i
Paths == {"main","copy","fork","foreign"}
History(i) == CASE i = "prefix" -> <<1>> [] i = "fork" -> <<1,3>>
                [] OTHER -> <<1,2>>
Compatible(a,b) == \A k \in (DOMAIN a \cap DOMAIN b): a[k] = b[k]
EventSet(s) == {s[k] : k \in DOMAIN s}
Rows == 1..3
Empty == [identity |-> "none", history |-> <<>>, facts |-> {}]
VARIABLES provider, rows, owner, observed
vars == <<provider, rows, owner, observed>>
Identity(i) == IF i = "foreign" THEN Other(provider) ELSE provider
Matches(i) == {r \in Rows : rows[r].identity = Identity(i)
                              /\ Compatible(rows[r].history,History(i))}
Vacant == {r \in Rows : rows[r] = Empty}
\* Opaque row numbers abstract stable storage IDs and deterministic hash
\* suffix allocation. Prefer a compatible path owner, then a compatible row.
Targets(i) == IF owner[Path(i)] \in Matches(i) THEN {owner[Path(i)]}
              ELSE IF Matches(i) # {} THEN Matches(i)
              ELSE IF Vacant # {} THEN {CHOOSE r \in Vacant: \A s \in Vacant: r <= s}
              ELSE {}
Init ==
  /\ provider \in Domains /\ rows = [r \in Rows |-> Empty]
  /\ owner = [p \in Paths |-> 0] /\ observed = {}
Observe(i,r) ==
  /\ r \in Targets(i)
  /\ LET h == History(i)
         current == rows[r].history
         updated == [identity |-> Identity(i),
                     history |-> IF Len(h) > Len(current) THEN h ELSE current,
                     facts |-> rows[r].facts \cup EventSet(h)]
     IN rows' = [rows EXCEPT ![r] = updated]
  /\ owner' = [owner EXCEPT ![Path(i)] = r] /\ observed' = observed \cup {i}
  /\ UNCHANGED provider
Remove(p) ==
  /\ owner[p] # 0 /\ owner' = [owner EXCEPT ![p] = 0]
  /\ UNCHANGED <<provider,rows,observed>>
Next == (\E i \in Inputs, r \in Rows: Observe(i,r)) \/ (\E p \in Paths: Remove(p))
Spec == Init /\ [][Next]_vars

TypeOK ==
  /\ provider \in Domains /\ observed \subseteq Inputs /\ owner \in [Paths -> 0..3]
  /\ rows \in [Rows -> [identity : Domains \cup {"none"},
                        history : {<<>>,<<1>>,<<1,2>>,<<1,3>>}, facts : SUBSET {1,2,3}]]
\* Oracle uses distinct complete submitted histories, independently of the
\* resolver: replay/copy of either fork adds no row and loss adds no fact.
Complete == {i \in observed: i # "prefix"}
Required == {<<Identity(i),History(i)>> : i \in Complete}
SnapshotSet == {<<rows[r].identity,rows[r].history>> : r \in Rows}
Occupied == {r \in Rows: rows[r] # Empty}
ExpectedCount ==
  Cardinality(Required)
  + (IF "prefix" \in observed /\ (observed \cap {"main","fork","copy"}) = {} THEN 1 ELSE 0)
IdentityAndRetention == Required \subseteq SnapshotSet /\ Cardinality(Occupied) = ExpectedCount
ExactFacts == \A r \in Rows: rows[r].facts = EventSet(rows[r].history)
PathOwnership ==
  \A p \in Paths: owner[p] # 0 =>
    /\ rows[owner[p]].identity = Identity(p)
    /\ (p \in {"fork","copy","foreign"} => rows[owner[p]].history = History(p))
=============================================================================
