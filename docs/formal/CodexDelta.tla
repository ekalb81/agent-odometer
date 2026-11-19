---------------------------- MODULE CodexDelta -------------------------------
EXTENDS Integers, Sequences
\* One input dimension; Reconciliation checks read/write subset attribution.
\* A resumed file can contain cumulative counts, last-call counts or both.
Absent == -1
Count(total,last) == [changeModel |-> FALSE,cumulative |-> total,last |-> last]
ModelRecord == [changeModel |-> TRUE,cumulative |-> Absent,last |-> Absent]
Samples == <<
  <<Count(3,3), Count(5,2), ModelRecord, Count(5,2), Count(7,2)>>,
  <<Count(Absent,3), ModelRecord, Count(5,2), Count(5,Absent)>>,
  <<Count(0,0), Count(Absent,2), Count(2,2), ModelRecord, Count(6,1)>>
>>
\* Separate, literal oracles record contributions at their original record
\* offsets and the model actually known there. Zero-delta records create no fact.
Expected == <<
  << <<1,0,3>>, <<2,0,2>>, <<5,1,2>> >>,
  << <<1,0,3>>, <<3,1,2>> >>,
  << <<2,0,2>>, <<5,1,4>> >>
>>
VARIABLES sample, cursor, model, accounted, buckets, events
vars == <<sample,cursor,model,accounted,buckets,events>>
Init == /\ sample \in DOMAIN Samples /\ cursor = 0 /\ model = 0
        /\ accounted = 0 /\ buckets = <<0,0>> /\ events = <<>>
Step ==
  /\ cursor < Len(Samples[sample]) /\ cursor' = cursor + 1
  /\ LET record == Samples[sample][cursor']
     IN IF record.changeModel THEN
          /\ model' = 1 /\ UNCHANGED <<accounted,buckets,events>>
        ELSE
          LET baseline == accounted
              delta == IF record.cumulative = Absent THEN record.last
                       ELSE IF record.cumulative > baseline THEN record.cumulative-baseline ELSE 0
          IN /\ events' = IF delta = 0 THEN events ELSE Append(events,<<cursor',model,delta>>)
             /\ buckets' = [buckets EXCEPT ![model+1] = @ + delta]
             /\ accounted' = accounted + delta /\ UNCHANGED model
  /\ UNCHANGED sample
Next == Step \/ (cursor = Len(Samples[sample]) /\ UNCHANGED vars)
Spec == Init /\ [][Next]_vars
RECURSIVE Sum(_)
Sum(seq) == IF seq = <<>> THEN 0 ELSE Head(seq)[3] + Sum(Tail(seq))
EventOracle ==
  LET expected == SelectSeq(Expected[sample],LAMBDA e: e[1] <= cursor)
  IN /\ events = expected /\ accounted = Sum(expected)
     /\ \A m \in 0..1: buckets[m+1] = Sum(SelectSeq(expected,LAMBDA e: e[2] = m))
=============================================================================
