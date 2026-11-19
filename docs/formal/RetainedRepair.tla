-------------------------- MODULE RetainedRepair ---------------------------
EXTENDS Integers, Sequences

\* Schema 17 flags the archived session. The source has disappeared.
\* The fixture follows input, cache read, cache write, output, reasoning, total.
Zero == <<0,0,0,0,0,0>>
First == <<2,0,1,1,0,3>>
Second == <<1,1,0,1,1,2>>
Repeat == <<1,0,0,1,0,2>>
WrongAdvance == <<1,1,0,0,0,1>>
Expected == <<3,1,1,2,1,5>>
WrongCache == <<3,0,1,2,1,5>>
Cumulative == <<3,3,5>>
Old == <<First,Repeat,Second>>
Repaired == <<First,Zero,Second>>
LegacyKeys == <<"legacy-0","legacy-1","legacy-2">>
Unavailable == -1
Cases == {"verified","gap","wrong-subset","turn-mismatch","snapshot-mismatch"}
Base(scenario) == IF scenario = "gap" THEN <<First,Repeat,WrongAdvance>> ELSE Old
Reported(scenario) == IF scenario = "wrong-subset" THEN WrongCache ELSE Expected
Prior(i) == IF i = 1 THEN 0 ELSE Cumulative[i-1]
Normalize(facts) ==
  [i \in 1..3 |-> IF Cumulative[i] = Prior(i) THEN Zero ELSE facts[i]]
Total(facts) ==
  [d \in 1..6 |-> facts[1][d] + facts[2][d] + facts[3][d]]
StepsMatch(facts) ==
  \A i \in 1..3: Cumulative[i] = Prior(i) \/ facts[i][6] = Cumulative[i]-Prior(i)
Hours(facts) == <<facts[1][6],facts[2][6],facts[3][6]>>
Turns(facts) == <<[d \in 1..6 |-> facts[1][d] + facts[2][d]],facts[3]>>

VARIABLES scenario, ledger, reading
vars == <<scenario,ledger,reading>>

Init ==
  /\ scenario \in Cases
  /\ LET old == Base(scenario)
     IN ledger = [schema |-> 17, dirty |-> TRUE, facts |-> old, keys |-> LegacyKeys,
                  snapshot |-> old, turns |-> Turns(old), hours |-> Hours(old)]
  /\ reading = Unavailable

Migrate ==
  /\ ledger.schema = 17
  /\ LET fresh == Normalize(ledger.facts)
         agrees == Total(fresh) = Reported(scenario)
         proved == StepsMatch(ledger.facts) /\ agrees
                   /\ scenario \notin {"turn-mismatch","snapshot-mismatch"}
     IN ledger' = IF proved
          THEN [ledger EXCEPT !.schema = 18, !.dirty = FALSE, !.facts = fresh,
                    !.snapshot = fresh, !.turns = Turns(fresh), !.hours = Hours(fresh)]
          ELSE [ledger EXCEPT !.schema = 18]
  /\ UNCHANGED <<scenario,reading>>

Read ==
  /\ ledger.schema = 18
  /\ reading' = IF ledger.dirty THEN Unavailable
                ELSE ledger.facts[1][6] + ledger.hours[2] + ledger.facts[3][6]
  /\ UNCHANGED <<scenario,ledger>>

EvidencePreserved ==
  /\ ledger.keys = LegacyKeys
  /\ ledger.schema = 18 /\ scenario # "verified" =>
       /\ ledger.dirty /\ ledger.facts = Base(scenario)
       /\ ledger.snapshot = Base(scenario) /\ reading = Unavailable

RepairedAccounting ==
  ~ledger.dirty =>
    /\ ledger.schema = 18 /\ scenario = "verified"
    /\ ledger.facts = Repaired /\ ledger.snapshot = Repaired
    /\ ledger.turns = <<First,Second>> /\ ledger.hours = <<3,0,2>>
    /\ reading \in {Unavailable,5}

Next == Migrate \/ Read \/ UNCHANGED vars
Spec == Init /\ [][Next]_vars
=============================================================================
