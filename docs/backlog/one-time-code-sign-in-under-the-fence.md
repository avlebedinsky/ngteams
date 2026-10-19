---
worth: later
added: 2026-10-20
---
# Can `proto-lab login` enter a one-time code under the fence?

Found by the review of M0 Task 8 (`docs/plans/20261019-native-teams-m0-lab-and-spikes.md`). The plan asked `login` to
write `need-code` to `status-<alias>.json` and wait for the code in `inbox/<alias>.code` when Microsoft asks for one.
Entering the code needs Entra's multi-factor posts on `login.microsoftonline.com` (`SAS/BeginAuth`, `SAS/EndAuth`,
`SAS/ProcessAuth`), and `Classifier::sign_in` denies them, so the code could never be submitted. `login` now stops at a
one-time-code prompt (`SignInStop::MfaCode`) like at every other multi-factor prompt; nothing needs undoing.

Adding those posts to the own-state list needs evidence nobody has yet; the decision was taken with Codex
(transcript under the run's `discussions/fixer-task8-otc/`):
- what each request does: BeginAuth starts the challenge as soon as the page shows the prompt (a text, a voice call or
  an Authenticator push for the default method), before the driver could stop on it; who receives it (the account's
  registered device, and whose that is);
- how the method is chosen (the request bodies), since the classifier matches method, origin and path only, and an
  own allow skips the membership check;
- that the same paths never carry registration or security-info changes, which must stay denied.

The unknown that settles it: a capture of an Entra one-time-code sign-in for a lab account, taken under the armed fence
with these posts denied (Task 9's recorder), reviewed before any rule is written. Until then an account that Microsoft
asks for a code can't be signed in by `login`. No lab account has been asked for one so far (2026-10-19).
