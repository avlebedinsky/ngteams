---
worth: later
added: 2026-10-21
---
# The sign-in window's guard misses two kinds of redirect

Found by M2 Task 24 (`docs/plans/20261021-native-teams-m2-gpui-daily-driver.md`), decided with Codex (transcript
under the run's `discussions/task-24-webkit-guard/`), sources read at WebKit's `webkitgtk-2.54.1` tag. `teams-signin`'s
guard sits in a web process extension: each page's `send-request` hands the request's method and URL to the guard,
which logs its decision to `fence.log` and answers before the request goes. Two kinds of request never reach it:

- **A synchronous load's redirect.** The network process follows it itself (`NetworkResourceLoader.cpp:1438–1449`), with
  no second `send-request`. The window test pins it: a synchronous `GET` answered by a 302 reaches its target with no
  decision, and a synchronous `POST` the guard allows (a test-only local own-state rule standing in for Entra's
  sign-in form) answered by a 307 reaches an unclassified endpoint as a `POST`. The same `POST` sent asynchronously has
  its redirect decided and denied.
- **A kept-alive load's redirect after its page is gone** (`NetworkResourceLoader.cpp:1476–1483`): the window test sees
  a kept-alive `GET` redirected 1.5 s after the page navigated away reach its target with no decision.

`NetworkLoadChecker` doesn't apply content rules on those paths (`NetworkLoadChecker.h:199`, `.cpp:625–628`), a content
rule list couldn't log a decision anyway, and WebKitGTK 2.54.1 has no setting that turns synchronous XHR or keepalive
off. Either gap breaks "every decision logged before the request goes", so the guard isn't a complete fence and
`teams-signin` opens only a local page (`teams_signin::cli::NOT_LIVE`); the lab signs in through `proto-lab auth`.

What would close it, neither tried yet:
- a mandatory, inspecting egress guard: the only way out of every WebKit subprocess that sends (the network process
  above all), with no direct connection and no uninspected tunnel possible, seeing method, URL and body; for HTTPS that
  means terminating TLS (the network session's proxy settings and `allow_tls_certificate_for_host` give a starting
  point, not a proof), and it needs its own coverage proof, as the Chrome fence's arming has;
- or a WebKit change that hands every redirect back to the web process (or to the UI process) before it is followed.

Until then the personal build's sign-in (Task 26) has to choose another route or accept the gaps knowingly; the user
decides that with Task 26's rules.
