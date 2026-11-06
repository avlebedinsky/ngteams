//! A synthetic calendar service for view tests, including replies held across navigation or a save.

use std::collections::{HashMap, VecDeque};

use teams_core::{
    Answer, CalendarEvent, EngineError, EventWrite, EventWritten, FullEvent, Invitable, MeetingAvailability, Reply,
};

#[derive(Default)]
pub struct CalendarFake {
    pub events: Vec<CalendarEvent>,
    pub reads: Vec<(i64, i64)>,
    /// What the store kept: the stored read's answer.
    pub stored: Vec<CalendarEvent>,
    pub stored_reads: Vec<(i64, i64)>,
    pub answers: VecDeque<Result<Vec<CalendarEvent>, EngineError>>,
    pub hold: bool,
    pub held: Vec<Answer<Vec<CalendarEvent>>>,
    pub full: HashMap<String, FullEvent>,
    pub full_reads: Vec<String>,
    pub writes: Vec<EventWrite>,
    pub write_answers: VecDeque<Result<EventWritten, EngineError>>,
    pub hold_writes: bool,
    pub held_writes: Vec<Answer<EventWritten>>,
    pub people: Vec<Invitable>,
    pub people_queries: Vec<String>,
    pub availability: MeetingAvailability,
    /// The Meet now titles asked, each with its channel.
    pub titles: Vec<(String, String)>,
    /// What the people's calendars read answers, and each read asked (the addresses, the start, the days).
    pub schedules: Vec<teams_proto::PersonSchedule>,
    pub schedule_reads: Vec<(Vec<String>, i64, u32)>,
    /// The calendar's Meet now: the meeting it makes (none: it fails), and each name asked.
    pub meet_now: Option<teams_proto::MeetNowMeeting>,
    pub meet_now_asked: Vec<String>,
}

impl CalendarFake {
    pub fn read(&mut self, start_ms: i64, end_ms: i64) -> Reply<Vec<CalendarEvent>> {
        self.reads.push((start_ms, end_ms));
        if self.hold {
            let (answer, reply) = Reply::later();
            self.held.push(answer);
            reply
        } else {
            Reply::ready(self.answers.pop_front().unwrap_or_else(|| Ok(self.events.clone())))
        }
    }

    pub fn stored(&mut self, start_ms: i64, end_ms: i64) -> Reply<Vec<CalendarEvent>> {
        self.stored_reads.push((start_ms, end_ms));
        Reply::ready(Ok(self.stored.clone()))
    }

    pub fn write(&mut self, write: EventWrite) -> Reply<EventWritten> {
        let kind = write.kind();
        self.writes.push(write);
        if self.hold_writes {
            let (answer, reply) = Reply::later();
            self.held_writes.push(answer);
            reply
        } else {
            let answer = self.write_answers.pop_front().unwrap_or_else(|| {
                Err(EngineError::EventNotSaved {
                    kind,
                    why: "nothing saves here".to_owned(),
                })
            });
            // as the engine does before it answers: the store forgets what the write touched
            if answer.is_ok() {
                let touched = self.writes.last().map(EventWrite::touched).unwrap_or_default();
                self.stored.retain(|event| !touched.touches(event));
            }
            Reply::ready(answer)
        }
    }
}
