----------------------------- MODULE Watcher -------------------------------
EXTENDS Naturals

\* One path, two root generations, one bulk callback per generation. "empty"
\* represents a successful enumeration that did not find this path. "error"
\* represents an incomplete scan and cannot establish disappearance.
Signals == {"none","present","missing"}
Result == {"found","empty","error"}
Stages == {"idle","read","reserved","written","done"}
VARIABLES registered, callback, gen, job, stage, result, touch, live, source, history, everRecorded,
          justifiedMissing
vars == <<registered,callback,gen,job,stage,result,touch,live,source,history,everRecorded,justifiedMissing>>
View(g,s) == [generation |-> g, signal |-> s]
Idle == [origin |-> 0, generation |-> 0, signal |-> "none", stage |-> "idle"]
Init ==
  /\ registered = {1} /\ callback = Idle
  /\ gen = 1 /\ job = 0 /\ stage = "idle" /\ result = "error"
  /\ touch = "none" /\ live = View(1,"none")
  /\ source \in {"present","missing"} /\ history \in BOOLEAN
  /\ (source = "present" => history) /\ everRecorded = history
  /\ justifiedMissing = (source = "missing")
Begin(r) ==
  /\ job < gen /\ stage \in {"idle","done"}
  /\ job' = gen /\ result' = r /\ stage' = "read"
  /\ UNCHANGED <<gen,touch,live,source,history,everRecorded,justifiedMissing>>
Settings ==
  /\ gen = 1 /\ stage \notin {"reserved","written"} \* settings-transition lock
  /\ gen' = 2 /\ touch' = "none" /\ live' = View(2,"none")
  /\ UNCHANGED <<job,stage,result,source,history,everRecorded,justifiedMissing>>
InstallHandler ==
  /\ gen \notin registered /\ registered' = registered \cup {gen}
  /\ UNCHANGED <<callback,gen,job,stage,result,touch,live,source,history,everRecorded,justifiedMissing>>
\* OS callbacks can run after the handle was replaced. Retain the token
\* captured at construction even if delivery or disk I/O starts much later.
DeliverHandler(g,s) ==
  /\ g \in registered /\ callback = Idle
  /\ callback' = [origin |-> g, generation |-> g, signal |-> s, stage |-> "delivered"]
  /\ UNCHANGED <<registered,gen,job,stage,result,touch,live,source,history,everRecorded,justifiedMissing>>
ReserveHandler ==
  /\ callback.stage = "delivered"
  /\ callback' = IF callback.generation = gen THEN [callback EXCEPT !.stage = "reserved"] ELSE Idle
  /\ UNCHANGED <<registered,gen,job,stage,result,touch,live,source,history,everRecorded,justifiedMissing>>
Watch(s) ==
  /\ touch' = s /\ live' = View(gen,s) /\ source' = s
  /\ history' = (history \/ s = "present") /\ everRecorded' = (everRecorded \/ s = "present")
  /\ justifiedMissing' = (s = "missing")
  /\ UNCHANGED <<gen,job,stage,result>>
Inspect ==
  /\ stage = "read"
  /\ LET current == job = gen /\ touch = "none"
         missing == current /\ result = "empty"
     IN /\ stage' = IF current /\ result = "found" THEN "reserved" ELSE "done"
        /\ live' = IF missing THEN View(gen,"missing") ELSE live
        /\ source' = IF missing THEN "missing" ELSE source
        /\ justifiedMissing' = (justifiedMissing \/ (job = gen /\ touch = "none" /\ result = "empty"))
  /\ UNCHANGED <<gen,job,result,touch,history,everRecorded>>
Commit ==
  /\ stage = "reserved" /\ source' = "present" /\ stage' = "written"
  /\ history' = TRUE /\ everRecorded' = TRUE
  /\ UNCHANGED <<gen,job,result,touch,live,justifiedMissing>>
CorrectAndPublish ==
  /\ stage = "written" /\ stage' = "done"
  /\ source' = IF touch = "missing" THEN "missing" ELSE "present"
  /\ live' = IF touch = "none" THEN View(job,"present") ELSE live
  /\ justifiedMissing' = (touch = "missing")
  /\ UNCHANGED <<gen,job,result,touch,history,everRecorded>>
FinishHandler ==
  /\ callback.stage = "reserved"
  /\ callback' = Idle /\ UNCHANGED registered
  /\ IF callback.generation = gen THEN Watch(callback.signal)
     ELSE UNCHANGED <<gen,job,stage,result,touch,live,source,history,everRecorded,justifiedMissing>>
Advance == (Inspect \/ Commit \/ CorrectAndPublish) /\ UNCHANGED <<registered,callback>>
Next == \/ ((\E r \in Result: Begin(r)) \/ Settings \/ Advance) /\ UNCHANGED <<registered,callback>>
        \/ InstallHandler \/ ReserveHandler \/ FinishHandler
        \/ \E g \in registered, s \in {"present","missing"}: DeliverHandler(g,s)
Spec == Init /\ [][Next]_vars /\ WF_vars(Advance)
TypeOK ==
  /\ gen \in 1..2 /\ job \in 0..gen /\ stage \in Stages /\ result \in Result
  /\ touch \in Signals /\ live \in [generation: 1..2, signal: Signals]
  /\ source \in {"present","missing"} /\ history \in BOOLEAN
  /\ everRecorded \in BOOLEAN /\ justifiedMissing \in BOOLEAN
CallbackFence ==
  [] [(callback # Idle /\ callback.origin # gen /\ callback' = Idle =>
        UNCHANGED <<touch,live,source,history,everRecorded,justifiedMissing>>)]_vars
CurrentCallbackPublishes ==
  [] [(callback.stage = "reserved" /\ callback.origin = gen /\ callback' = Idle =>
        live' = View(gen,callback.signal) /\ source' = callback.signal)]_vars
LiveGeneration == live.generation = gen
WatcherWins == touch # "none" => live.signal = touch
SettledAvailability == touch # "none" /\ stage # "written" => source = touch
NoInventedMissing == source = "missing" => justifiedMissing
Retained == everRecorded => history
\* A writer can briefly race a tombstone between commit and correction.
\* Weak fairness applies only to the scan worker. No watcher traffic or user
\* action is required to finish the correction.
Finishes == (stage \in {"read","reserved","written"}) ~> (stage = "done")
=============================================================================
