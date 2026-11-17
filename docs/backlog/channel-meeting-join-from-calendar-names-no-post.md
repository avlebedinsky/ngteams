# A channel meeting's Join from the calendar or the banner names no post

The Join of a channel meeting from the calendar card, "Join with an ID", the started-meeting banner, and the ctl
`calendar`/`links` commands sends `MeetingToJoin::Chat { chat: <channel>, post: None }`. The engine then joins the
first active meetup that CSA names in the channel, else the newest unended call start in its posts. The organizer of
the event is not used. When two meetings run in one channel at the same time (a scheduled meeting X and a Meet now Y),
the Join of X from these places can go into Y, while the windows show the title of X. The Join on the post and the
Join in the channel header already name the post (`CallAction::JoinChannel { post }`).

Before a fix, capture a scheduled channel meeting while it runs, to find which id each place holds:

- the `messageId` of its CSA `activeMeetups` entry;
- the id of its `Event/Call` `callStarted` post (a reply under the meeting's post, or a root post);
- the `mid` of the event's `skypeTeamsData` (`ChannelEventRef::post`) and the post segment of its `meetup-join` link.

Then send the id that both `CsaTeams::meetup_of_post` and `ChannelMeetup::of_post_in` find. The banner has the
`callStarted` id (`Pending::start`). The calendar has only the meeting's root post. If the ids do not agree, match the
meetup by its organizer (the event's `MeetingInfo`) instead. Add a `teams-core` case with two meetups in one channel.
A wrong id must not stop the Join of the only meeting in a channel: that Join works now.
