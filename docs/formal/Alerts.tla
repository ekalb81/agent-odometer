------------------------------- MODULE Alerts -------------------------------
\* Two representative, independently deduplicated conditions in one collector:
\* a shared ambient candidate and one exact consent-bound live account rule.
Keys == {"ambient", "account"}
\* The account rule is configured and enabled. Master opt-in covers both
\* conditions, and the ambient candidate also needs its category enabled.
OptInSettings == {{}, {"account"}, Keys}
Observations ==
    [ambient |-> {"unknown", "resolved", "active", "quiet-active", "expired-active"},
     account |-> {"unknown", "resolved", "active", "quiet-active", "rate-limited",
                  "wrong-consent"}]
Evidence(observation) ==
    IF observation \in {"rate-limited", "wrong-consent"} THEN "unknown"
    ELSE observation
Active(value) == value \in {"active", "quiet-active", "expired-active"}
Results == {"saved", "write-failed", "revision-conflict"}
Idle == [status |-> "idle"]
Receipt(status, notices) == [status |-> status, notices |-> notices]

VARIABLES optIn, quiet, baseline, failed, log, pending, receipt, displayed
vars == <<optIn, quiet, baseline, failed, log, pending, receipt, displayed>>
Init ==
    /\ optIn \in OptInSettings
    /\ quiet = FALSE
    /\ baseline = FALSE
    /\ failed = FALSE
    /\ log = {}
    /\ pending = Idle
    /\ receipt = Receipt("none", {})
    /\ displayed = FALSE

ChangePolicy(enabled) ==
    /\ pending.status = "idle" /\ enabled # optIn
    /\ optIn' = enabled
    /\ baseline' = FALSE
    /\ UNCHANGED <<quiet, failed, log, pending, receipt, displayed>>

QuietHours(current) ==
    /\ pending.status = "idle" /\ current # quiet
    /\ quiet' = current
    /\ baseline' = IF quiet /\ ~current THEN FALSE ELSE baseline
    /\ UNCHANGED <<optIn, failed, log, pending, receipt, displayed>>

Resume ==
    /\ pending.status = "idle"
    /\ baseline' = FALSE
    /\ UNCHANGED <<optIn, quiet, failed, log, pending, receipt, displayed>>

NextLog(values) ==
    {k \in Keys : Active(values[k]) \/ (k \in log /\ values[k] # "resolved")}
ProposedNotices(values) ==
    {k \in optIn : values[k] = "active" /\ k \notin log /\ baseline /\ ~failed /\ ~quiet}

\* The collector holds the evaluation lock from snapshot through persistence.
\* Taking another collection or changing policy waits for the pending save.
Begin(values) ==
    /\ pending.status = "idle"
    /\ LET sample == [k \in Keys |-> Evidence(values[k])]
       IN pending' = [status |-> "writing", sampled |-> values,
                      nextLog |-> NextLog(sample), notices |-> ProposedNotices(sample)]
    /\ receipt' = Receipt("checking", {})
    /\ UNCHANGED <<optIn, quiet, baseline, failed, log, displayed>>

Finish(result) ==
    /\ pending.status = "writing"
    /\ IF result = "saved"
       THEN /\ log' = pending.nextLog
            /\ receipt' = Receipt(result, pending.notices)
            /\ failed' = FALSE
       ELSE /\ UNCHANGED log
            /\ receipt' = Receipt(result, {})
            /\ failed' = TRUE
    /\ baseline' = TRUE
    /\ pending' = Idle
    /\ UNCHANGED <<optIn, quiet, displayed>>

\* The marker makes a presentation read visible to TLC without touching the
\* configuration, condition keys, pending write, or delivery receipt.
Present ==
    /\ displayed' = ~displayed
    /\ UNCHANGED <<optIn, quiet, baseline, failed, log, pending, receipt>>

Samples == { [ambient |-> a, account |-> b] :
             a \in Observations.ambient, b \in Observations.account }
Next ==
    \/ \E enabled \in OptInSettings : ChangePolicy(enabled)
    \/ \E current \in BOOLEAN : QuietHours(current)
    \/ Resume
    \/ \E values \in Samples : Begin(values)
    \/ \E result \in Results : Finish(result)
    \/ Present
Spec == Init /\ [][Next]_vars

NonConsumingPresentation ==
    [][(displayed' # displayed) =>
        UNCHANGED <<optIn, quiet, baseline, failed, log, pending, receipt>>]_vars

SerializedCollection ==
    [][(pending.status = "writing") =>
        /\ optIn' = optIn /\ quiet' = quiet
        /\ pending'.status = "writing" => pending' = pending /\ log' = log]_vars

DeliveredAfterSave ==
    [][(receipt' # receipt /\ receipt'.notices # {}) =>
        /\ pending.status = "writing" /\ pending'.status = "idle"
        /\ receipt'.status = "saved" /\ baseline /\ ~failed /\ ~quiet
        /\ \A k \in receipt'.notices :
              /\ k \in optIn /\ k \notin log /\ k \in log'
              /\ pending.sampled[k] = "active"]_vars

EligibleCrossingsDeliver ==
    [][(pending.status = "writing" /\ pending'.status = "idle" /\ receipt'.status = "saved" /\
        baseline /\ ~failed /\ ~quiet) =>
        \A k \in optIn : (pending.sampled[k] = "active" /\ k \notin log) =>
                            k \in receipt'.notices]_vars

SaveFailureKeepsLog ==
    [][(pending.status = "writing" /\ pending'.status = "idle" /\ receipt'.status # "saved") =>
        /\ log' = log /\ receipt'.notices = {} /\ failed']_vars

KnownEdgesConsumed ==
    [][(pending.status = "writing" /\ pending'.status = "idle" /\ receipt'.status = "saved") =>
        /\ \A k \in Keys :
              /\ Active(pending.sampled[k]) => k \in log'
              /\ pending.sampled[k] \in {"unknown", "rate-limited", "wrong-consent"} =>
                   (k \in log') = (k \in log)
              /\ pending.sampled[k] = "resolved" => k \notin log'
        /\ ~baseline \/ failed => receipt'.notices = {}]_vars
=============================================================================
