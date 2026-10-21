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

While `NOT_LIVE` holds the window stays on loopback for its whole run, not only its first URL (a review of Task 24):
the guard denies every request off a numeric loopback address (`Guard::local_only`), the navigation policy refuses
every navigation off one, and the network session reaches anything else only through `teams_signin::egress`'s
black hole, a proxy on loopback that logs and refuses every connection (the custom proxy settings put it under every
load of the soup session, the gaps above included). The window test's `remote` scenario pins it: the gaps' redirects
to this machine's own non-loopback address reach the black hole, and nothing reaches the listeners there. That is a
bound for the offline window, not the egress guard below: the black hole refuses, it doesn't inspect and forward.

Arming is checked for the view's first page only. A page's extension tells the guard it is armed (with its process
id), the authorize URL opens only once the view's own page is, and a page with no armed record is denied; but a later
web process (a process swap on a navigation, or one `WebKit` starts again) whose extension didn't load sends its
requests without asking the guard, and nothing notices: the process id is told but not used, and `WebKitGTK` names
no web process to the UI process to compare it with. The window ends its run when the page's web process ends
(`WEB_PROCESS_ENDED`), which covers a crash but not a swap. Off loopback the black hole still refuses such a process's
requests.

What would close it, neither tried yet:
- a mandatory, inspecting egress guard: the only way out of every WebKit subprocess that sends (the network process
  above all), with no direct connection and no uninspected tunnel possible, seeing method, URL and body; for HTTPS that
  means terminating TLS (the network session's proxy settings and `allow_tls_certificate_for_host` give a starting
  point, not a proof), and it needs its own coverage proof, as the Chrome fence's arming has;
- or a WebKit change that hands every redirect back to the web process (or to the UI process) before it is followed.

Until then the personal build's sign-in (Task 26) has to choose another route or accept the gaps knowingly; the user
decides that with Task 26's rules.
