-------------------------- MODULE Reconciliation --------------------------
EXTENDS Naturals, Sequences

\* These are input, read-cache and write-cache units, all nonnegative.
T(i,r,w) == [input |-> i, read |-> r, write |-> w]
Zero == T(0,0,0)
Plus(a,b) == T(a.input+b.input, a.read+b.read, a.write+b.write)
SmallCalls == {Zero,T(1,0,0),T(1,1,0),T(1,0,1)}
Carry == {T(0,0,0),T(1,1,0),T(1,0,0)}
Sat(a,b) == IF a >= b THEN a-b ELSE 0
Min(a,b) == IF a < b THEN a ELSE b
Disjoint(t) == t.read + t.write <= t.input
VARIABLES step, bucket, events, evidence
vars == <<step,bucket,events,evidence>>
Init == /\ step = 0 /\ bucket = <<Zero,Zero>> /\ events = <<>> /\ evidence = <<>>
TokenCount(m,call,carry,late) ==
  /\ step < 2
  /\ LET base == Plus(Plus(bucket[1],bucket[2]),call)
         cumulative == T(base.input+carry.input,base.read+carry.read,base.write+late)
         remainderIn == Sat(cumulative.input,base.input)
         remainderRead == Sat(cumulative.read,base.read)
         extra == Min(Sat(cumulative.write,base.write),Sat(remainderIn,remainderRead))
         delta == Plus(call,T(remainderIn,remainderRead,extra))
     IN /\ Disjoint(cumulative)
        /\ events' = Append(events,delta)
        /\ evidence' = Append(evidence,[direct |-> call, carry |-> carry, late |-> late])
        /\ bucket' = [bucket EXCEPT ![m] = Plus(@,delta)]
  /\ step' = step+1
Next == (\E m \in 1..2, c \in SmallCalls, r \in Carry, l \in 0..2: TokenCount(m,c,r,l))
        \/ (step = 2 /\ UNCHANGED vars)
Spec == Init /\ [][Next]_vars
Sum(e) == CASE Len(e) = 0 -> Zero [] Len(e) = 1 -> e[1] [] OTHER -> Plus(e[1],e[2])
Conservation == Plus(bucket[1],bucket[2]) = Sum(events)
\* An independent constraint characterization: every accepted delayed unit fits
\* unassigned plain input in THIS event. If any delayed evidence is discarded,
\* the new plain input capacity must already be completely consumed.
LocalEvidence ==
  \A i \in DOMAIN events:
    LET d == events[i]
        x == evidence[i]
    IN /\ Disjoint(d)
       /\ d.input = x.direct.input + x.carry.input /\ d.read = x.direct.read + x.carry.read
       /\ d.write >= x.direct.write /\ d.write <= x.direct.write+x.late
       /\ d.write-x.direct.write <= x.carry.input-x.carry.read
       /\ (d.write < x.direct.write+x.late =>
               d.write-x.direct.write = x.carry.input-x.carry.read)
=============================================================================
