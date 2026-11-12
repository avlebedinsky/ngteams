# Loki, the persona card service

How browser Teams' profile card (the hover card and the full card on a person's picture) reads a person, and how
`ngteams` reads the same. Seen in a lab capture of 2026-11-11 (lab accounts a and b opening each other's cards and c's
in browser Teams) and checked the same day with plain `GET`s and lab account a's own native token, reading lab
account c only. No value from either is copied here; ids, names and addresses below are placeholders.

The card's data comes from Loki, Microsoft's persona card service (Delve's "live persona card"), plus the presence
service (UPS) for availability and last seen. Everything here is a read: nothing in it reaches the person read.

## The token

An Entra token for the resource `https://loki.delve.office.com` (scope `https://loki.delve.office.com/.default`). Its
`aud` is not that URL but Loki's application ID (a GUID), and it carries the scopes
`LLM.Read Organization.Read User.Read User.Read.All`. Our native client ID gets it from the account's refresh token
like any other audience ([auth.md](auth.md#audience--service-table)). The transport asks for it as
`LOKI_AUDIENCE` for both of Loki's bases.

## The configuration and the regional base

`GET https://loki.delve.office.com/api/v1/livepersonacard/configuration?cultureName=en-us` answers a JSON object
about the card for this account: `LokiUrl` (the account's regional base, `https://nam.loki.delve.office.com/` for the
lab), the account's and tenant's object ids, its UPN and display name, flights, settings, telemetry collectors and
the card's script resources. Only `LokiUrl` is read.

- Every request needs `X-ClientType`; without it Loki answers 400 `ClientTypeMissing`.
- The global host answers the configuration only: an API path there is 403 `DisallowedPathOnTopLevelDomain`. The
  configuration also answers on the regional host.
- The native client reads it once per engine, the first time a card is asked for (again after a failure), and hands
  the base to the reads' transport (`HttpTransport::set_loki_region`), which takes only
  `https://<one label>.loki.delve.office.com/` and only once. Before that a regional read is refused, not sent.

## The web client's form and ours

The web card sends each read as a CORS "simple request": a `POST` with `Content-Type: text/plain`, whose body is a
JSON object holding what would be headers (`Accept`, `X-ClientType: Teams`, `X-ClientFeature: LivePersonaCard`,
`X-ClientArchitectureVersion: v2`, `X-ClientScenario` naming the card's part, `X-HostAppPlatform: Web`,
`X-LPCVersion`, the bearer token, correlation ids), so the browser sends no preflight. The same reads answer 200 as
plain `GET`s with those as real headers, and that is the native form: `Authorization: Bearer <token>`,
`Accept: application/json`, `X-ClientType: Teams`, `X-ClientFeature: LivePersonaCard`,
`X-ClientArchitectureVersion: v2`, `X-HostAppPlatform: Web`, no body. `X-ClientScenario`, `X-LPCVersion` and the
correlation ids are not needed and not sent.

The web card's query on every person read is `teamsMri=8:orgid:<oid>&smtp=<mail>&userPrincipalName=<upn>&
personaType=User&aadObjectId=<oid>&displayName=<name>` plus its own persona and correlation ids. The minimal query
answers the same: `teamsMri=8:orgid:<oid>&aadObjectId=<oid>&personaType=User`, which is all the native client sends
(so a card needs only the MRI). A person read is made only for an `8:orgid:<GUID>` MRI.

## The reads

All on the regional base, all `GET`, each answered `application/json` (`teams_proto::loki`).

- **Person**, `api/v2/person?<query>` (the web card adds `locale=en-us`): `{"person": {"names": [{"value":
  {"displayName", "givenName", "surname"}, "source": "Organisation"}], "emailAddresses": [{"value": {"name",
  "address", "type": "Main"}, …}], "imAddresses": [{"value": {"displayText", "url": "sip:…"}, …}], "workDetails":
  [{"value": {}, …}], "enhancedWorkDetails": {}, "userPrincipalName", "aadObjectId", "isContact", "userType":
  "Member", "recipientType", "recipientTypeDetails", "isAgenticUser", …}}`. Read: the first name entry's three
  parts, every mail address and IM display text, the UPN and the user type (`LokiPerson`); an answer naming another
  `aadObjectId` is refused. **Unverified:** `workDetails`' keys (job title, department, company, office) and phone
  numbers: the lab accounts have none, so they aren't read.
- **Organization**, `api/v1/organization?<query>`: `{"managers": [], "directs": [], "dottedLineManagers": []}`.
  **Unverified:** no capture showed an entry. `LokiOrganization` reads each manager's and direct report's entry
  leniently with the working-with entry's keys below and drops one without an object id; dotted-line managers aren't
  read. The web card also posts `api/v1/organization/manager`, answered 204 when there is none.
- **Working with**, `api/v1/workingwith?<query>&limit=12`: `{"value": [{"aadObjectId", "fullName", "email",
  "userPrincipalName", "profilePictureUrl", "permanentEntryId"}]}`, the people the card's Organization tab shows under
  "<name> works with". `LokiWorkingWith` keeps up to 12, each by its object id (MRI `8:orgid:<oid>`), name, mail and
  UPN.
- **Schedule**, `api/v1/schedule?<query>`: `{"nextAvailability": {"utcDateTime", "currentStatus", "nextStatus"},
  "workingHoursCalendar": {"daysOfWeek": ["Monday", …], "startTime": "08:00:00.0000000", "endTime":
  "17:00:00.0000000", "timeZone": {"offset": 3.0, "name": "GMT+03:00", "odataType":
  "#microsoft.graph.customTimeZone"}}}`. `utcDateTime` is UTC without an offset; `0001-01-01T00:00:00` means no
  time. The status words are Graph's `freeBusyStatus` (`free`, `tentative`, `busy`, `oof`, `workingElsewhere`,
  `unknown`), only `Free` captured; another word is kept as given. The offset is in hours and may be fractional;
  only a custom zone was captured. The web card showed "Free on 11/12" at 22:02 local, outside working hours: when
  free, it names the next working day. `LokiSchedule` reads all of it.
- **Out of office**, `api/v1/oofstatus?<query>`: `{"outOfOfficeState": "Disabled", "externalAudience": "Unknown",
  "emailAddress"}`. Only `Disabled` was captured; `LokiOutOfOffice` keeps any other state as given rather than
  reading it as on.

## With presence and the short profile

The card's availability line ("Last seen 4 hours ago • Free on 11/12", "Offline", "Available") combines UPS and the
schedule. UPS's presence read and pushes carry `lastActiveTime` (RFC 3339 UTC; `0001-01-01T00:00:00Z` for never:
"Offline", not "Last seen") and `calendarData.isOutOfOffice` ([trouter.md](trouter.md#others-presence)).

`EngineRequests::profile_card` (`teams-core` `profile_card.rs`) reads, all at once: Loki's person, schedule, out of
office, organization and working-with; one UPS presence read of the person (`PresenceRead`); and the middle tier's
short profile (`ShortProfileLookup`), whose `jobTitle` and `officeLocation` are the card's job title and office, and
whose names and addresses stand in when Loki's person read fails. A section whose read failed is left out; every read
failing is an error. Nothing is stored.

## What isn't read

The web card also reads, all empty for the lab: `api/v1/peopleInsights/peoplecard` (`{"value": []}`),
`api/v2/extendeduserinfo` (skills, topics, interests, schools, birthday), `api/v1/extendeduserinfo/upacustomproperties`
(`[]`), `api/v2/graphql` (badges, the org explorer), `api/v2/linkedin/profiles` (the LinkedIn tab's matches by name)
and `api/v2/personaphoto` (404 without a photo). None of them is sent natively; pictures come from Graph
([graph.md](graph.md)).
