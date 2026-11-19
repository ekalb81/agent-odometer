------------------------------- MODULE Pricing -------------------------------
EXTENDS Integers

(* query.rs: price_tokens, token_cost, tier_multiplier
   query_desktop.rs: time_aware_surface
   rates.rs: RateCard::resolve_model_pricing
   SessionsView.svelte: runTableRefresh and table effect
   App.svelte: runTrayRefresh and tray effect

   One price bucket and one serialized frontend refresh. Card keys represent
   object identities. The replacement has the same version and doubles the
   synthetic rates. A verified scope and data revision are part of the reply's
   authority. Scope changes and replacement of verified data are atomic here.

   Amounts are in half-millionths of the reported unit, avoiding float rounding.
   Cache-write rates assume the documented reference durations (30 minutes for
   Codex API USD and five minutes for Claude USD). Usage has a cache-write count,
   with no per-event cache duration dimension in this model or the query input.
   The dated GPT-6.2 Astra API example uses the two covered sides of the March 1,
   2027 08:00 UTC change. The value -1 represents missing request-input evidence
   in these examples.
*)

NoPrice == <<0, "none", "unavailable", FALSE>>
Failure == <<0, "none", "request failed", FALSE>>
Cards == <<[version |-> 15, factor |-> 1], [version |-> 15, factor |-> 2]>>

(* Flat Plan, current purchased credits, API estimate, and dated API USD. *)
Credits == {"plan", "purchased"}
Api == {"api", "dated"}
Units(harness, surface) ==
    IF harness = "codex" /\ surface \in Credits THEN "credits" ELSE "USD"

Resolution(model, surface) ==
    CASE model = "no-rate" -> "unavailable"
      [] model = "free-local" /\ surface # "dated" -> "free_local"
      [] model = "alias" -> "aliased"
      [] model \in {"astra", "astra-62", "sol", "claude"} -> "direct"
      [] model = "unknown" /\ surface # "dated" -> "fallback"
      [] OTHER -> "unavailable"

TierTwice(harness, model, basis, surface, tier, period) ==
    CASE tier \in {"default", "standard"} -> 2
      [] basis = "fallback" -> 0
      [] tier = "ultrafast" /\ model = "astra-62" /\ surface = "dated" ->
            IF period = "before change" THEN 6 ELSE 12
      [] tier \in {"priority", "fast"} /\ harness = "codex" /\ surface # "dated"
            /\ model \in {"astra", "alias", "sol"} ->
            IF surface = "plan" THEN 5 ELSE 4
      [] OTHER -> 0

(* Ordinary input, cache reads, cache writes, ordinary output, reasoning.
   An explicit Codex purchased-credit surface has a zero cache-write rate. *)
Rate(harness, surface, basis) ==
    CASE basis = "fallback" /\ harness = "claude" -> <<2, 3, 4, 5, 6>>
      [] basis = "fallback" /\ surface = "purchased" -> <<1, 2, 0, 4, 5>>
      [] basis = "fallback" -> <<1, 2, 3, 4, 5>>
      [] harness = "claude" -> <<6, 3, 9, 15, 17>>
      [] surface = "purchased" -> <<5, 2, 0, 11, 13>>
      [] surface = "plan" -> <<5, 2, 7, 11, 13>>
      [] OTHER -> <<4, 1, 8, 12, 14>>

SatSub(a, b) == IF a >= b THEN a - b ELSE 0
CostTwice(tokens, rate, longRequest) ==
    LET input == SatSub(SatSub(tokens.input, tokens.read), tokens.write) * rate[1]
                      + tokens.read * rate[2] + tokens.write * rate[3]
        output == SatSub(tokens.output, tokens.reasoning) * rate[4]
                      + tokens.reasoning * rate[5]
    IN IF longRequest THEN 4 * input + 3 * output ELSE 2 * (input + output)

Price(card, harness, model, surface, tier, period, observedInput, tokens) ==
    LET basis == Resolution(model, surface)
        tierTwice == TierTwice(harness, model, basis, surface, tier, period)
        unit == Units(harness, surface)
        thresholdApplies == surface = "dated" /\ harness = "codex"
        evidenceMissing == thresholdApplies /\ observedInput = -1
        isLong == IF thresholdApplies /\ ~evidenceMissing
                  THEN observedInput > 272000 ELSE FALSE
    IN CASE harness = "claude" /\ surface \in {"api", "purchased"} -> NoPrice
          [] basis = "unavailable" -> NoPrice
          [] basis = "free_local" -> <<0, unit, basis, FALSE>>
          [] tierTwice = 0 -> NoPrice
          [] OTHER -> <<(CostTwice(tokens, Rate(harness, surface, basis), isLong)
                        * tierTwice * Cards[card].factor) \div 2,
                        unit, basis, evidenceMissing>>

Tokens(scope, data) == [input |-> 3 + scope + data, read |-> 1, write |-> 1,
                        output |-> 2, reasoning |-> 1]

(* Fixed expected outputs for the component model, including total/subset
   arithmetic, dated boundaries, and unavailable vs declared zero. *)
Examples == {
    <<"codex", "astra", "plan", "standard", "before change", 0, <<76, "credits", "direct", FALSE>>>>,
    <<"codex", "alias", "plan", "priority", "before change", 0, <<190, "credits", "aliased", FALSE>>>>,
    <<"codex", "astra", "api", "fast", "before change", 0, <<156, "USD", "direct", FALSE>>>>,
    <<"codex", "astra", "purchased", "fast", "before change", 0, <<124, "credits", "direct", FALSE>>>>,
    <<"codex", "unknown", "purchased", "default", "before change", 0, <<24, "credits", "fallback", FALSE>>>>,
    <<"claude", "claude", "plan", "standard", "before change", 0, <<96, "USD", "direct", FALSE>>>>,
    <<"claude", "unknown", "plan", "standard", "before change", 0, <<38, "USD", "fallback", FALSE>>>>,
    <<"claude", "claude", "api", "standard", "before change", 0, NoPrice>>,
    <<"claude", "claude", "plan", "priority", "before change", 0, NoPrice>>,
    <<"codex", "no-rate", "plan", "standard", "before change", 0, NoPrice>>,
    <<"codex", "free-local", "plan", "standard", "before change", 0, <<0, "credits", "free_local", FALSE>>>>,
    <<"codex", "unknown", "plan", "fast", "before change", 0, NoPrice>>,
    <<"codex", "unknown", "dated", "standard", "before change", 0, NoPrice>>,
    <<"codex", "sol", "purchased", "ultrafast", "before change", 0, NoPrice>>,
    <<"codex", "astra", "plan", "mystery", "before change", 0, NoPrice>>,
    <<"codex", "astra-62", "dated", "standard", "before change", -1, <<78, "USD", "direct", TRUE>>>>,
    <<"codex", "astra-62", "dated", "standard", "before change", 0, <<78, "USD", "direct", FALSE>>>>,
    <<"codex", "astra-62", "dated", "standard", "before change", 272000, <<78, "USD", "direct", FALSE>>>>,
    <<"codex", "astra-62", "dated", "standard", "before change", 272001, <<130, "USD", "direct", FALSE>>>>,
    <<"codex", "astra-62", "dated", "ultrafast", "before change", -1, <<234, "USD", "direct", TRUE>>>>,
    <<"codex", "astra-62", "dated", "ultrafast", "at change", -1, <<468, "USD", "direct", TRUE>>>>,
    <<"codex", "astra-62", "dated", "ultrafast", "at change", 272001, <<780, "USD", "direct", FALSE>>>>
}

ExamplesMatch == \A example \in Examples :
    LET tokens == IF example[1] = "claude" THEN [Tokens(0, 0) EXCEPT !.reasoning = 0]
                  ELSE Tokens(0, 0)
    IN Price(1, example[1], example[2], example[3], example[4], example[5], example[6], tokens)
        = example[7]

(* Reconciled event deltas can have a subset larger than its parent counter.
   Saturate the ordinary remainder per event and retain the observed subsets. *)
ASSUME CostTwice([input |-> 1, read |-> 1, write |-> 1, output |-> 0, reasoning |-> 1],
                 <<5, 2, 7, 11, 13>>, FALSE) = 44

VARIABLES card, scope, data, request, raw, publication
vars == <<card, scope, data, request, raw, publication>>
NoRequest == [card |-> 0, scope |-> 0, data |-> 0]
NoPublication == [key |-> NoRequest, result |-> NoPrice]
NoRaw == [scope |-> 2, data |-> 2, tokens |-> Tokens(0, 0)]
Key == [card |-> card, scope |-> scope, data |-> data]
VerifiedRaw == [scope |-> scope, data |-> data, tokens |-> Tokens(scope, data)]
BatchPrice(cardId, tokens) ==
    Price(cardId, "codex", "astra", "api", "fast", "before change", 0, tokens)

Init ==
    /\ card = 1 /\ scope = 0 /\ data = 0
    /\ request = NoRequest /\ raw = NoRaw /\ publication = NoPublication

Request ==
    /\ request = NoRequest
    /\ request' = Key
    /\ UNCHANGED <<card, scope, data, raw, publication>>

ReplaceCard ==
    /\ card = 1 /\ card' = 2
    /\ publication' = NoPublication
    /\ UNCHANGED <<scope, data, request, raw>>

ChangeScope ==
    /\ scope = 0 /\ scope' = 1
    /\ raw' = NoRaw /\ publication' = NoPublication
    /\ UNCHANGED <<card, data, request>>

ChangeData ==
    /\ data = 0 /\ data' = 1
    /\ raw' = NoRaw /\ publication' = NoPublication
    /\ UNCHANGED <<card, scope, request>>

Complete(ok) ==
    /\ request # NoRequest /\ request' = NoRequest
    /\ IF request = Key THEN
           IF ok THEN
               /\ raw' = VerifiedRaw
               /\ publication' = [key |-> request,
                                    result |-> BatchPrice(request.card, Tokens(request.scope, request.data))]
           ELSE
               /\ raw' = NoRaw
               /\ publication' = [key |-> request, result |-> Failure]
       ELSE UNCHANGED <<raw, publication>>
    /\ UNCHANGED <<card, scope, data>>

Next == Request \/ ReplaceCard \/ ChangeScope \/ ChangeData \/ Complete(TRUE) \/ Complete(FALSE)
Spec == Init /\ [][Next]_vars

FreshPublication ==
    IF publication = NoPublication THEN TRUE ELSE
        /\ publication.key = Key
        /\ IF publication.result = Failure THEN raw = NoRaw
           ELSE raw # NoRaw /\ publication.result = BatchPrice(card, raw.tokens)

VerifiedTokensOnly == raw = NoRaw \/ raw = VerifiedRaw

SameVersionEditPreservesRaw ==
    [][card' # card =>
        /\ Cards[card'].version = Cards[card].version
        /\ raw' = raw /\ publication' = NoPublication]_vars

SupersededReplyDoesNotPublish ==
    [][(request # NoRequest /\ request' = NoRequest /\ request # Key)
        => UNCHANGED <<raw, publication>>]_vars

=============================================================================
