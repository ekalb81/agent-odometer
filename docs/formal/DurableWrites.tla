--------------------------- MODULE DurableWrites ----------------------------
EXTENDS Naturals, Sequences, FiniteSets
\* File copies may contain identical usage histories from three distinct first
\* turns under the same provider ID. Preserve the first allocated public key.
Turns == {"first-A","first-B","first-C"}
Inputs == Turns \X {"original","copy"}
Lineage == "identical-usage"
Base == <<"provider/session","base",Lineage>>
Collision(fingerprint) == <<"provider/session",fingerprint,Lineage>>
Keys == {Base} \cup {Collision(f): f \in Turns}
NoRow == "unused"
VARIABLES promotion, scenario, owners, observed, bindings, cursor, facts, hours, metadata
vars == <<promotion,scenario,owners,observed,bindings,cursor,facts,hours,metadata>>
(* Version 17 can retain last-only facts before a final fingerprint exists.
   These fixtures hold the provider ID constant; integers stand for full
   canonical event signatures, including timestamp and usage. *)
Provisional == <<3,2>>
PromotionInputs == [continued |-> <<3,2,10>>, divergent |-> <<3,9,10>>, shorter |-> <<10>>]
SiblingInputs == [saved |-> Provisional, changed |-> <<3,9>>]
BoundArtifact == "bound-prefix-copy"
NoReplay == [kind |-> "none", artifact |-> "none", metadataMatches |-> FALSE, owner |-> "none"]
PendingPromotion == [kind |-> "pending", path |-> "same", metadataMatches |-> TRUE,
                     owner |-> "original", original |-> Provisional, replacement |-> <<>>, replay |-> NoReplay]
Init ==
  /\ scenario \in {"identity","events","promotion"} /\ owners = [k \in Keys |-> NoRow]
  /\ observed = {} /\ bindings = {} /\ cursor = 0 /\ facts = {} /\ hours = <<0,0,0>>
  /\ metadata = {} /\ promotion = PendingPromotion
Observe(input) ==
  /\ scenario = "identity"
  /\ LET f == input[1]
         matches == {k \in Keys: owners[k] = f}
         chosen == IF matches # {} THEN CHOOSE k \in matches: TRUE
                   ELSE IF owners[Base] = NoRow THEN Base ELSE Collision(f)
     IN /\ owners' = IF owners[chosen] \in {NoRow,f} THEN [owners EXCEPT ![chosen] = f] ELSE owners
        /\ bindings' = bindings \cup {<<input,chosen>>}
  /\ observed' = observed \cup {input}
  /\ UNCHANGED <<scenario,cursor,facts,hours,metadata>>
IdentitySound ==
  /\ \A input \in observed: \E k \in Keys: <<input,k>> \in bindings /\ owners[k] = input[1]
  /\ \A f \in Turns: Cardinality({k \in Keys: owners[k] = f}) <= 1
  /\ \A b \in bindings: owners[b[2]] = b[1][1]

\* Each item is an attempted insertion at a new source offset. Event key 2
\* is replayed with and without supplemental request-input metadata. Its
\* accounted time, token amount and identity stay fixed.
Attempts == << <<1,FALSE>>, <<2,FALSE>>, <<2,TRUE>>, <<3,FALSE>>, <<2,FALSE>> >>
When == <<1,2,4>>
Amount == <<3,5,7>>
Store ==
  /\ scenario = "events" /\ cursor < Len(Attempts) /\ cursor' = cursor+1
  /\ LET attempt == Attempts[cursor']
         k == attempt[1]
         hour == When[k] \div 2 + 1
         inserted == k \notin facts
     IN /\ facts' = facts \cup {k}
        /\ metadata' = IF attempt[2] THEN metadata \cup {k} ELSE metadata
        /\ hours' = IF inserted THEN [hours EXCEPT ![hour] = @ + Amount[k]] ELSE hours
  /\ UNCHANGED <<scenario,owners,observed,bindings>>
\* Window [1,4] crosses two partial hours and the full hour containing key 2.
Windows == {<<0,1>>,<<2,3>>,<<4,5>>,<<1,4>>,<<2,2>>,<<0,5>>}
Raw(window,k) == k \in facts /\ window[1] <= When[k] /\ When[k] <= window[2]
Full(window,h) == window[1] <= (h-1)*2 /\ (h-1)*2+1 <= window[2]
Query(window,h) ==
  IF Full(window,h) THEN hours[h] ELSE IF Raw(window,h) THEN Amount[h] ELSE 0
FactKeyAccounting ==
  /\ facts = (CASE cursor = 0 -> {} [] cursor = 1 -> {1} [] cursor <= 3 -> {1,2} [] OTHER -> {1,2,3})
  /\ metadata = IF cursor >= 3 THEN {2} ELSE {}
  /\ \A w \in Windows:
       Query(w,1)+Query(w,2)+Query(w,3) =
         (IF cursor >= 1 /\ w[1] <= 1 /\ 1 <= w[2] THEN 3 ELSE 0) +
         (IF cursor >= 2 /\ w[1] <= 2 /\ 2 <= w[2] THEN 5 ELSE 0) +
         (IF cursor >= 4 /\ w[1] <= 4 /\ 4 <= w[2] THEN 7 ELSE 0)
Promote(kind,path,metadataMatches) ==
  /\ scenario = "promotion" /\ promotion.kind = "pending"
  /\ LET incoming == PromotionInputs[kind]
         retained == promotion.original
         prefixMatches == Len(retained) <= Len(incoming)
                          /\ SubSeq(incoming,1,Len(retained)) = retained
         reuse == metadataMatches /\ prefixMatches
         key == IF reuse THEN "original" ELSE "replacement"
         prior == promotion[key]
         suffix == SubSeq(incoming,Len(prior)+1,Len(incoming))
     IN promotion' = [promotion EXCEPT !.kind = kind, !.path = path,
           !.metadataMatches = metadataMatches, !.owner = key, ![key] = prior \o suffix]
  /\ UNCHANGED <<scenario,owners,observed,bindings,cursor,facts,hours,metadata>>
PromotionAccounting ==
  promotion.kind # "pending" /\ promotion.replay = NoReplay =>
    LET incoming == PromotionInputs[promotion.kind]
    IN IF promotion.kind = "continued" /\ promotion.metadataMatches
       THEN promotion.owner = "original" /\ promotion.original = incoming /\ promotion.replacement = <<>>
       ELSE promotion.owner = "replacement" /\ promotion.original = Provisional /\ promotion.replacement = incoming
(* Before promotion the original and its copy both belonged to one row.
   After the original finalizes, a rescan may still supply the shorter copy. *)
ReplaySibling(kind,artifact,metadataMatches) ==
  /\ scenario = "promotion" /\ promotion.kind = "continued" /\ promotion.owner = "original"
  /\ promotion.replay = NoReplay
  /\ LET incoming == SiblingInputs[kind]
         samePrefix == SubSeq(promotion.original,1,Len(incoming)) = incoming
         reuseBound == artifact = BoundArtifact /\ metadataMatches /\ samePrefix
         key == IF reuseBound THEN "original" ELSE "replacement"
     IN promotion' = [promotion EXCEPT
         !.replay = [kind |-> kind, artifact |-> artifact, metadataMatches |-> metadataMatches, owner |-> key],
         !.replacement = IF reuseBound THEN @ ELSE incoming]
  /\ UNCHANGED <<scenario,owners,observed,bindings,cursor,facts,hours,metadata>>
SiblingAccounting ==
  LET replay == promotion.replay
  IN replay # NoReplay =>
       /\ promotion.original = PromotionInputs.continued
       /\ IF replay.kind = "saved" /\ replay.artifact = BoundArtifact /\ replay.metadataMatches
          THEN replay.owner = "original" /\ promotion.replacement = <<>>
          ELSE replay.owner = "replacement" /\ promotion.replacement = SiblingInputs[replay.kind]
Next == \/ ((\E i \in Inputs: Observe(i)) \/ Store) /\ UNCHANGED promotion
        \/ \E kind \in DOMAIN PromotionInputs, path \in {"same","moved"}, m \in BOOLEAN: Promote(kind,path,m)
        \/ \E k \in DOMAIN SiblingInputs, a \in {BoundArtifact,"other"}, m \in BOOLEAN: ReplaySibling(k,a,m)
        \/ UNCHANGED vars
Spec == Init /\ [][Next]_vars
=============================================================================
