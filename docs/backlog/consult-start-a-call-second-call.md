---
worth: soon
where: crates/teams-call/src/machine.rs, crates/teams-core/src/calling.rs, crates/teams-core/src/call_floor.rs, crates/teams-ui/src/calls.rs
added: 2026-11-06
---
# Consult's "Start a call" and the consultative transfer need a second call while the first is held

Teams' Consult (1:1 calls) holds the call, then "Start a call" calls the person picked while the first call stays held
in a floating card (its name, "Hold" and how long, Resume, Leave); the consult call's toolbar leads with an outlined
Transfer, which hands the held call's far end to the consulted person. The whole exchange is captured
(`docs/protocol/calling.md`, "More of a running call" → Consult). The native client stops at the dialog's Open chat:

- `teams_call::Calls` (the machine) keeps one `call: Option<Call>`; every `CallAction` names a `CallId` but the
  machine refuses one that isn't its call (`CallError::NotNow`), and an invitation while a call runs is declined busy.
- `teams-core::Calling` keeps one `live: Option<LiveMedia>` (one libwebrtc session, one gallery, one dialect).
- The call floor (`call_floor.rs`) admits one call or ring across the app's accounts.
- The UI's `Calls` model and the call window show one active call.

## Plan

1. **Machine**: `call: Option<Call>` → a small map of calls by `CallId` with at most one *active* (not held) call.
   Deliveries are already routed by callback link (`track.rs` binds each call's links), so the lookup becomes
   per-call instead of "the call". A start (`CallAction::Call`) while the only other call is `Held` by the account is
   allowed and marked `consult_of: Some(held)`, its start body naming `endpointMetadata.consultationContext:
   {"isIncomingConsult": true}`. Busy rules stay for every other case.
2. **Media**: `Calling::live` → `HashMap<CallId, LiveMedia>`; the device seam (microphone, speaker, camera) goes to
   the active call only: the held call's session keeps its lines (its microphone already sends silence while held).
   Two libwebrtc peers at once are fine in libwebrtc; the capture threads must be shared, not doubled
   (`MediaSession` takes the devices on start: give the consult session the devices only once the held one released
   them, or route frames by the active id).
3. **Floor**: admit a second call of the same account while its first is held (the floor stays one *active* call
   across accounts).
4. **Consultative transfer** (`CallAction::Transfer { call: held, target, consult: Some(consult) }`): renegotiate the
   consult call `inactive` (`callModalities: []`), then post the held call's `transfer` with
   `replacementDetails: {replaces: <consult far end's `replacement` link from its acceptance>,
   replacementTargetParticipantId}`; on `transferAcceptance` leave the held call; the consult call ends with it.
   The transferred side (`isConsultative: true`) is the blind path plus the replacement fields; the target side is a
   `replacement` delivery at the consult leg's callback (`callNotification`): take it as the consult leg's
   successor (a retarget-like move of the call to the new call id, media renegotiated as the new call's offer says).
5. **UI**: the floating held-call card (name, "Hold" and its time, Resume, Leave) over the consult call's window, the
   consult call's toolbar with Transfer first (outlined), Resume on the card making that call active again (holding
   the other one).

Each step lands on its own behind the existing mock (`teams-mock` gains a second call and the replacement delivery);
the live check needs a's native client and two others (b and c) answering.
